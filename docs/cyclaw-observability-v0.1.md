# cyClaw 可观测性设计 v0.1

## 定位

cyClaw 的可观测性目标是回答三个问题：

1. **一次 Agent 工作发生了什么**：从 run 开始到模型审查结束，每一步的状态、耗时和失败原因。
2. **一次大模型推理是怎么发生的**：哪个 Provider、哪次调用、消耗多少 Token、重试了几次、是否命中缓存、失败在哪一步。
3. **一次命令执行对知识资产意味着什么**：退出码、超时、Git commit、会话与链路关联，以及失败是否进入了候选层。

cyClaw 是本地工具，因此可观测性同样遵循项目总原则：**本地优先、append-only、正文脱敏、观测写入不阻断主链路**。不引入 tracing/OpenTelemetry 运行时依赖，也不要求任何 collector 或网络上报；数据全部落在 `.cyclaw/` 下的 JSONL 文件，格式与现有事件日志一致，未来需要时可离线转换为 OTLP。

## 数据模型

可观测性由三层记录组成，均为追加式 JSONL：

| 记录 | 文件 | 说明 |
| --- | --- | --- |
| `TraceSpan` | `.cyclaw/traces.jsonl` | 链路中的一个 span：trace_id / span_id / parent_span_id 构成树，带状态、起止时间、耗时和属性 |
| `ModelCallRecord` | `.cyclaw/model-calls.jsonl` | 大模型推理明细：Provider、模型、阶段、Token、延迟、尝试次数、缓存命中、脱敏输入输出预览、错误摘要 |
| `ExecutionEvent` | `.cyclaw/execution-events.jsonl` | 已有执行事件，新增与 trace 的显式关联（`session_id`、`trace_id` 字段此前已存在） |

三者通过 ID 关联：`ModelCallRecord` 和 `ExecutionEvent` 都携带 `trace_id`；模型调用的 `span_id` 即 `traces.jsonl` 中 `kind=model_inference` span 的 `span_id`。

```text
trace_id（Agent run 时等于 run_id）
└── span: agent_run（root，kind=agent_run）
    └── span: model_review（kind=agent_run）
        └── span: model_call（kind=model_inference）
            └── model-calls.jsonl 中的一条推理明细
```

命令执行链路：`cyclaw exec` 传入或生成 `trace_id`，结束时写入一条 `kind=exec` 的根 span，并与 `execution-events.jsonl` 中同一 `trace_id` 的执行事件互查。

### Span 状态

`TraceSpanStatus` 为 `ok` / `error` / `timeout`；`TraceSpanKind` 目前为 `agent_run` / `exec` / `model_inference` / `mcp` / `task`。模型调用 span 的状态遵循底层调用结果：成功与缓存命中为 `ok`，重试耗尽或解析失败为 `error`。

### 示例记录

```json
// .cyclaw/traces.jsonl
{"schema_version":1,"trace_id":"agent-19c3f2","span_id":"span-1a2b","parent_span_id":"span-9f8e","name":"model_call","kind":"model_inference","source":"cyclaw-model","status":"ok","started_at":"2026-09-23T08:41:02+00:00","ended_at":"2026-09-23T08:41:03+00:00","duration_millis":812,"attributes":{"provider":"deepseek","model":"deepseek-v4-flash","phase":"agent_review","input_tokens":540,"output_tokens":96,"attempts":1,"cache_hit":false}}
```

```json
// .cyclaw/model-calls.jsonl
{"schema_version":1,"id":"model_call-77a1","trace_id":"agent-19c3f2","span_id":"span-1a2b","provider":"deepseek","model":"deepseek-v4-flash","phase":"agent_review","status":"success","input_tokens":540,"output_tokens":96,"total_tokens":636,"latency_millis":812,"attempts":1,"prompt_preview":"请作为 cyClaw 项目知识 Agent……","response_preview":"{\"reviews\":[……","error_summary":null,"started_at":"2026-09-23T08:41:02+00:00","finished_at":"2026-09-23T08:41:03+00:00"}
```

## 链路贯通点

| 入口 | trace 行为 |
| --- | --- |
| `cyclaw agent run` | `run_id` 兼作 `trace_id`；结束时写 `agent_run` 根 span；模型审查写 `model_review` 子 span；底层推理写 `model_call` span 与明细 |
| `cyclaw exec` | `--trace-id` 沿用外部链路，缺省时生成新 trace；正常/超时退出各写一条 `exec` span（状态分别为 ok/error 与 timeout） |
| `cyclaw model test` | 未传链路时生成独立 trace，输出中提示对应的 `trace show` 命令 |
| MCP `record_execution_event` | 支持可选 `session_id`、`trace_id`、`duration_millis`，与 CLI 路径对称 |
| Agent 事件 | `ModelCalled` 事件携带 `trace_id` 与模型调用 `span_id`；`AgentRunCompleted` 携带 `trace_id`；Agent 运行记录 JSON 内含 `trace_id` 字段 |

## 查询面

### CLI

```powershell
# 最近的链路索引：span 数、错误数、总耗时
cyclaw trace list --limit 20

# 还原一条完整链路：span 树 + 推理路径 + 关联执行事件
cyclaw trace show <trace_id>        # Agent 运行记录的 run_id 即 trace_id

# 大模型推理明细（推理路径回放）
cyclaw model calls list --limit 20
cyclaw model calls list --provider deepseek
```

### MCP 工具

| 工具 | 说明 |
| --- | --- |
| `list_traces` | 按 `trace_id` 聚合 span 数量、错误数与总耗时，按开始时间倒序 |
| `get_trace` | 按 `trace_id` 返回 span 树、模型推理明细和关联执行事件 |
| `list_model_calls` | 返回推理调用明细，可按 `provider` 过滤 |

三个工具均为只读幂等工具；Coding Agent 可直接用 `get_trace` 回放一次失败任务，或用 `list_model_calls` 审计 Token 消耗。

## 隐私与安全边界

- **正文不落盘**：`model-calls.jsonl` 只保存预览（输入、输出、错误摘要各最多 240 字符）；Prompt 在进入预览前已经过与实际请求一致的 `redact_sensitive_text` 脱敏。
- **不新增网络行为**：所有记录只写本地文件，没有任何上报。
- **观测失败不阻断**：span 与明细写入失败时静默丢弃本次记录（`let _ = ...`），推理、审查和命令执行结果不受影响。这是可观测性与审计事件（写入失败会导致操作失败）刻意区分的取舍。
- **与日预算并存**：`model-calls.jsonl` 是逐次明细，`.cyclaw/model-usage.json` 仍是按日汇总与预算控制的唯一依据，两者不互相替代。

## 设计取舍

- **为什么不用 `tracing` crate**：技术架构中规划过 `tracing`，但 cyClaw 的消费方是 CLI 人读输出、MCP 工具和未来 IDE 展示，而不是 OpenTelemetry exporter。JSONL 与现有事件、账本同构，工具链（读取、滚动、损坏处理）可直接复用；待需要分布式场景时，可加一层 `traces.jsonl -> OTLP` 的导出器，成本远低于一开始引入运行时。
- **为什么 trace_id 复用 run_id**：Agent run 本身就是最自然的任务边界，一个 ID 同时标识 run 与 trace，`trace show <run_id>` 不需要额外映射。
- **为什么 exec 的 span 是根 span**：外部 Agent（Codex 等）通过 `--trace-id` 传入链路时，cyClaw 不假设其 span 拓扑，只保证自己的执行段可查；本地缺省生成 trace 时，exec 是唯一 span，根即全部。

## 已知限制与演进

- Agent run 中途崩溃时只有已写入的子 span（如 `model_call`），没有根 span；`agent-run-state.json` 的恢复机制不受影响。
- Observer 的维护循环（证据验证、知识对账）尚未产生 span，可按同一模式补齐。
- `TraceSpanKind` 目前为封闭枚举，新增链路类型需要小版本扩展。
- 无自动过期清理；`traces.jsonl` / `model-calls.jsonl` 会随时间增长，后续可按 `evidence-verifications` 的滚动归档模式裁剪。
