# cyClaw 可靠性设计 v0.1

## 定位

可靠性层回答一个问题：**一次失败的 AI 任务，如何在无需人工值守的情况下恢复**。cyClaw 的 AI 任务（模型审查）失败原因大多为瞬态故障——网络抖动、Provider 限流、日预算临时不足、响应超时。可靠性层把这些失败从"人工再跑一次"升级为"记录 → 退避 → 自动恢复重试 → 耗尽降级"。

与可观测性相同，可靠性遵循项目总原则：本地文件、原子写入、项目锁保护、不新增网络行为。

## 分层防御

| 层 | 机制 | 位置 |
| --- | --- | --- |
| 单次请求 | 指数退避重试（100ms 起，上限 2 次）、请求超时、并发槽 | `cyclaw-model::test_openai_compatible` |
| 单次执行 | 失败任务写入重试队列，等待退避后重试 | `cyclaw-agent` |
| 跨执行 | `run_due_retries` 按到期时间批量恢复执行 | CLI / MCP |
| 耗尽兜底 | 降级为人工处理并记录审计事件，候选保持待审 | 重试队列状态机 |
| 存储一致性 | Fact Patch 事务日志、恢复与阻塞（已有能力） | `cyclaw-memory` |
| 链路可查 | 重试事件与队列条目携带 trace_id | `cyclaw-events` |

## 重试队列

存储为 `.cyclaw/retry-queue.json`（整队列原子替换写入，读写需持有 `retry-queue` 项目锁）。

```json
{
  "schema_version": 1,
  "id": "retry-19d0",
  "kind": "model_review",
  "status": "pending",
  "payload": { "provider": "deepseek", "pending_candidate_ids": ["kc-1", "kc-2"] },
  "attempt_count": 2,
  "max_attempts": 5,
  "next_retry_at": "2026-09-23T15:10:00+00:00",
  "last_error": "模型请求失败，已重试 2 次: HTTP 429 ...",
  "trace_id": "agent-19cf",
  "created_at": "...", "updated_at": "...", "completed_at": null,
  "degraded": false, "degraded_reason": null
}
```

### 状态机

```text
（模型审查失败，首次）
        │ new_retry_task（attempt_count=1）
        ▼
    ┌────────┐  run_due_retries 开始执行   ┌─────────────┐
    │ Pending│ ──────────────────────────▶ │ InProgress  │
    └────────┘                             └──────┬──────┘
        ▲                              审查成功 │      │ 审查失败
        │ next_retry_at 到期                    ▼      ▼
        │ attempt < max                   ┌─────────┐  attempt += 1
        │                                 │Completed│  ├─ attempt < max → Pending（退避重排）
        └─────────────────────────────────┴─────────┘  └─ attempt ≥ max → Exhausted（降级）
                                                          │
    人工 abandon_retry ──────────────────────────────▶ Abandoned
```

- **入队**：`agent run` 的模型审查失败时，若不存在对应条目则自动创建（`attempt_count=1`，`next_retry_at = now + 60s`）。审查成功不产生条目。
- **重试执行**：`run_due_retries` 筛选 `status=pending` 且 `next_retry_at` 到期的条目，重建 Agent 运行上下文后重新执行完整审查流程（候选重新收集，成功即完成）。
- **退避**：第 n 次尝试后延迟 `min(60s * 2^(n-1), 3600s)`——60s、120s、240s、480s、960s，一小时封顶。
- **耗尽降级**：`attempt_count >= max_attempts`（默认 5，即 1 次首发 + 4 次重试）时置 `exhausted`、`degraded=true`。降级语义与项目安全边界一致：**候选知识保持 pending 状态，模型不写入任何文档，由人工审阅**。自动化的失败永远不会转化为未授权的写入。
- **人工放弃**：确认是逻辑错误（如模型持续返回未知候选）而非瞬态故障时，`abandon` 终止重试并同样记录降级。

### 审计事件

| 事件 | 时机 |
| --- | --- |
| `RetryTaskScheduled` | 首次失败入队 |
| `RetryTaskCompleted` | 重试成功 |
| `RetryTaskExhausted` | 重试耗尽或人工放弃（降级） |

三个事件均携带 `retry_id` 与首次失败的 `trace_id`，可通过 `cyclaw trace show <trace_id>` 回放原始失败现场。

## 使用方式

### CLI

```powershell
# 查看队列（支持 --status pending|exhausted|... 过滤）
cyclaw retry list
cyclaw retry list --status exhausted

# 立即执行所有到期重试（不等退避计时器）
cyclaw retry run

# 人工放弃某条任务
cyclaw retry abandon <retry-id>
```

`agent run` 与 `retry run` 内部完全同构：重试就是"带失败上下文的又一次完整 run"，因此重试产物（Agent 运行记录、模型调用明细、trace）与普通 run 一致，可通过 `cyclaw agent runs list` 和 `cyclaw trace list` 观察。

### MCP 工具

| 工具 | 类型 | 说明 |
| --- | --- | --- |
| `list_retries` | 只读 | 查看队列，支持 `status` 过滤 |
| `run_retries` | 写入 | 立即执行到期重试，返回逐条 outcome |
| `abandon_retry` | 写入 | 人工放弃 |

Coding Agent 可以在会话开始时调用 `list_retries` 发现上次遗留的失败任务，调用 `run_retries` 完成恢复——这就是"恢复之后异步重试"的入口。

## 项目级配置

重试参数位于 `ModelPolicy`，可在 `.cyclaw/config.yaml` 中按项目覆盖：

```yaml
model_policy:
  retry_base_delay_seconds: 60    # 退避基准
  retry_max_attempts: 5           # 总尝试上限（含首次）
  retry_max_backoff_seconds: 3600 # 单次退避上限
```

耗尽判定使用**当前策略的实时值**：调小 `retry_max_attempts` 后，存量待重试条目也会按新上限提前耗尽。

## Provider 降级链

Provider 可配置备用项：主 Provider 的 HTTP 调用失败（网络、HTTP 状态、响应解析）时，自动用备用 Provider 重试一次：

```powershell
cyclaw model add primary --base-url https://api.a.com --model a --api-key-env A_API_KEY --fallback backup
cyclaw model add backup --base-url https://api.b.com --model b --api-key-env B_API_KEY
```

- 降级只在单层内发生（备用 Provider 不再递归降级）；
- 两次调用（含失败）均写入推理明细与 trace，`fallback_from` 字段标记降级来源；
- 备用 Provider 也失败时返回**主 Provider 的错误**（保留根因）；
- 权限拒绝、日预算不足、输入超限为全局约束，不触发降级。

## Observer 自动恢复

`cyclaw observer run` 的周期维护在空闲期自动调用 `run_due_retries`（每周期最多 5 条），到期失败任务无需人工值守即可恢复；恢复失败仅记录观察器错误，不影响主循环。重试仍可随时通过 `cyclaw retry run` 或 MCP `run_retries` 手动触发。

## 设计取舍

- **为什么复用完整 run 而不是断点续传**：候选集在两次执行之间可能变化（Observer 会持续产生候选），重新收集候选再审查比快照重放更符合最终一致性；`agent-run-state.json` 的 run_id 恢复机制继续服务于"进程中断"场景，重试队列服务于"模型调用失败"场景，两者互补。
- **为什么整个队列原子替换而不是逐条追加**：队列条目少（失败任务稀疏），整写简单可靠，与 `model-usage.json` 同构；未来条目增多时可切换为 JSONL 追加 + 压缩。
- **为什么重试产生新 trace 而不续用原 trace**：重试是新的完整运行，拥有新的 run_id；`RetryTask.trace_id` 保留首次失败的链路 ID 作为因果根，`RetryTaskScheduled/Completed/Exhausted` 事件把两者串起来。
- **v0.1 范围**：仅覆盖 `model_review` 一种任务。命令执行失败已由候选层承接（无需重试），Fact Patch 由事务日志恢复（已有）。

## 已知限制与演进

- fallback 链只有一层；多级降级（A→B→C）暂不支持。
- Observer 自动恢复的周期跟随维护间隔（默认见 `observer run --maintenance-interval-seconds`），暂不支持独立的重试调度精度。
