# cyClaw Agent Runtime 设计 v0.1

日期：2026-07-09

## 1. 目标

cyClaw 的目标是成为：

> 本地优先、模型可插拔、权限分层、记忆可追溯、资源克制的项目知识 Agent Runtime。

它不是一个重型 AI 应用平台，也不是只会执行固定命令的 CLI 工具。用户提供自己的模型 Provider 和 API Key，cyClaw 负责：

- 自动发现项目变化。
- 捕获值得沉淀的非代码知识。
- 生成候选知识和文档草稿。
- 调用用户配置的大模型进行高价值判断。
- 通过权限系统控制读写、联网、模型调用和自动化行为。
- 通过 CLI、MCP、IDE 插件接入开发工作流。

## 2. 多层权限设置

权限分为 6 层：

| 层级 | 名称 | 默认 | 说明 |
| --- | --- | --- | --- |
| L0 | ReadOnly | 是 | 读取项目、Git diff、docs、`.cyclaw` |
| L1 | LocalKnowledgeWrite | 是 | 写 `.cyclaw/` 本地知识资产 |
| L2 | DocsWrite | 否 | 写 `docs/`，必须通过 patch |
| L3 | ModelCall | 否 | 调用用户配置的大模型 |
| L4 | Automation | 否 | 后台自动编排 |
| L5 | Shell | 否 | 外部命令和高危动作，当前阶段不开放 |

默认策略：

```text
默认 L1
用户确认后 L2
显式配置模型后 L3
L4 只做后台编排
L5 暂不开放
```

配置文件：

```text
.cyclaw/config.yaml
```

第一版新增 `cyclaw-policy`，提供：

- `PolicyConfig`
- `PermissionLevel`
- `PathGuard`
- `ModelCallPolicy`
- `WriteScopeGuard`

当前实现状态：

- `cyclaw policy show` 可查看策略。
- `cyclaw policy check` 可验证路径写入权限。
- `cyclaw draft apply` 已接入 DocsWrite 路径检查。
- `cyclaw model test` 已接入 ModelCall 权限检查。
- `cyclaw model add` 会作为用户显式授权动作，同步开启 `allow_model_call` 和 `allow_network`。

## 3. 记忆系统

记忆系统分为：

| 类型 | 路径 | 作用 |
| --- | --- | --- |
| Project Profile Memory | `.cyclaw/project-profile.json`、`.cyclaw/project.md` | 项目画像 |
| Change Memory | `.cyclaw/runs/*/change-analysis.json` | 变更记录 |
| Knowledge Inbox Memory | `.cyclaw/knowledge-inbox.jsonl` | 候选知识 |
| Document Patch Memory | `.cyclaw/doc-patches/*.json` | 文档草稿 |
| Agent Run Memory | `.cyclaw/agent-runs/*.json` | Agent 运行记录 |
| Event Memory | `.cyclaw/events.jsonl` | 统一事件日志 |
| Retrieval Memory | `.cyclaw/index.sqlite` | 本地检索 |

第一版新增 `cyclaw-events`，统一记录：

- `project_scanned`
- `git_changed`
- `knowledge_candidate_created`
- `document_patch_created`
- `model_called`
- `agent_run_completed`

当前实现状态：

- `cyclaw agent run --once` 会写入 `agent_run_completed`。
- Agent 调用模型成功后会写入 `model_called`。
- `cyclaw events list` 可查看事件日志。

## 4. 高并发能力

当前阶段不做云端式无限并发，而做本地安全并发：

```text
读任务并发
写任务串行
模型调用限流
索引后台队列
```

第一版策略：

- MCP 只读工具保持轻量。
- 写 `.cyclaw` 和 `docs` 通过权限检查。
- 模型调用默认单并发。
- 事件日志 append-only。

后续增加：

```text
.cyclaw/locks/write.lock
.cyclaw/locks/index.lock
.cyclaw/locks/model.lock
```

## 5. 速度快，能力强

cyClaw 采用三条路径：

| 路径 | 说明 |
| --- | --- |
| Fast Path | 规则判断，不调用模型 |
| Smart Path | 结构化解析，后续引入 AST / Tree-sitter |
| Model Path | 用户自带模型，只处理高价值判断 |

原则：

- 能不用模型就不用模型。
- 能发摘要就不发源码。
- 能发 diff summary 就不发完整 diff。
- 模型输出必须可校验。
- 模型不能绕过用户确认直接写文档。

## 6. 低资源占用

运行模型：

```text
CLI 单次执行
MCP stdio 按客户端启动
IDE 插件按需启动
watch 轻量常驻
```

避免当前阶段引入：

- 常驻数据库服务。
- 重型 Web Server。
- 内置向量数据库。
- Electron 客户端。

## 7. 当前编码计划

### P0：权限系统

新增：

```text
crates/cyclaw-policy
cyclaw policy show
cyclaw policy check
```

### P1：事件系统

新增：

```text
crates/cyclaw-events
.cyclaw/events.jsonl
```

### P2：Agent Runtime 接入

将 Agent 运行结果写入事件日志：

```text
agent_run_completed
```

模型调用结果后续写入：

```text
model_called
```

### P3：后续增强

- 文件锁。
- 模型调用缓存。
- 增量扫描。
- retention 清理。
- 结构化模型输出 Schema。

## 8. 自动文档管理权限

自动文档管理不是普通的文档写入开关，而是一个单独的高风险授权：

- `allow_docs_apply=true`：允许应用已确认的文档草稿。
- `allow_auto_apply_docs=true`：允许 Watch/Agent 自动生成并应用文档草稿。
- 两项必须同时为 `true`，自动管理才会生效。
- 默认均为 `false`；关闭时仍会自动收集知识、生成候选和草稿，但不会修改 `docs/`。
- 自动应用由 Core Watch 和 Agent 共用同一套阈值过滤；启用模型时由 Agent 在结构化审查后应用，避免越过模型结论。
- 所有自动写入均保留文档草稿状态和事件日志，可通过 Git diff 与 `.cyclaw/events.jsonl` 追溯。

自动写入还必须通过置信度门槛：

- 本地规则初始置信度：高 `90`、中 `75`、低 `55`。
- `automation.auto_apply_min_confidence` 默认值为 `90`。
- 启用模型能力时，候选必须经过结构化模型审查。
- 模型输出 `recommendation=keep` 且置信度达到阈值后，才允许自动生成并应用草稿。
- 自动流程只应用本轮通过门槛后新生成的草稿，不处理历史待应用草稿。

结构化模型输出：

```json
{
  "reviews": [
    {
      "candidate_id": "候选 ID",
      "recommendation": "keep",
      "confidence": 92,
      "rationale": "中文审查理由"
    }
  ]
}
```

## 8.1 Watch 运行模型

Watch 使用混合检测模型：

```text
操作系统文件事件 -> 600ms 事件合并 -> Git 内容快照 -> 增量知识分析
```

- 文件系统事件负责低延迟唤醒，空闲时不执行周期轮询。
- Git 快照负责去重，过滤编辑器临时事件和重复通知。
- `.git`、`.cyclaw`、`node_modules`、`target`、`build`、`dist` 和 `.gradle` 不进入处理链路。
- 事件通道容量固定为 256，事件风暴不会无限增长内存。
- 符号链接只记录签名，不读取链接目标。
- 超过 8MB 的文件使用大小和修改时间签名，不全量加载进内存。
- VS Code 未受信任工作区不执行 CLI、MCP 或自动 Watch。

启用命令：

```text
cyclaw policy enable-auto-docs
```

## 9. 本轮落地状态（2026-07-12）

本轮已完成以下运行时硬化：

- 权限策略新增 `cyclaw policy set`、`cyclaw policy enable-model` 和 `cyclaw policy enable-docs-apply`，权限开关会持久化到 `.cyclaw/config.yaml`。
- 文档草稿应用现在同时要求路径权限和 `allow_docs_apply=true`，默认不会静默写入项目文档。
- `.cyclaw/locks` 增加项目级独占文件锁，覆盖项目初始化、扫描、变更分析、收件箱、文档草稿、索引和模型调用，避免多进程互相覆盖产物。
- 模型响应增加最多 128 条的本地缓存，缓存键包含 Provider、模型、思考模式和提示词，不保存 API Key。
- MCP 增加 `get_policy`、`list_events`、`list_agent_runs` 三个只读工具，便于 Codex 在执行任务前读取权限和历史上下文。

常用权限命令：

```text
cyclaw policy show
cyclaw policy enable-model
cyclaw policy enable-docs-apply
cyclaw policy set allow_shell --enabled=false
```

在本轮继续开发前，剩余工作主要是模型运行时、增量索引、事件覆盖、Agent 记录管理和 IDEA 插件构建验证，具体状态见下节。

## 10. 本轮继续落地状态（2026-07-12）

- 模型调用使用可配置并发槽位，多个进程会排队等待可用槽位；支持请求超时、指数退避和失败重试。
- `model_policy` 支持读取 `max_concurrent_calls`、`request_timeout_seconds`、`max_retries`、`min_interval_millis`。
- 索引更新改为按路径和内容比较的增量更新，新增、修改和删除文档只更新受影响记录。
- `accept`、`ignore`、文档草稿生成、文档草稿应用均写入事件日志。
- 新增 `cyclaw agent runs list` 和 `cyclaw agent runs clean --keep <数量>`，用于读取和清理 Agent 运行记录。
- IDEA Wrapper JAR 已补齐，并已通过 `apps/idea\gradlew.bat buildPlugin` 本机构建验证；构建产物为 `apps/idea/build/distributions/cyclaw-idea-0.1.0.zip`。Wrapper 使用腾讯 Gradle 8.7 镜像以提高当前环境的可复现性。

## 11. Agent 集成与 Hooks（2026-07-19）

MCP 已从只读知识接口扩展为受控闭环：

```text
doctor -> analyze_changes -> candidate detail/review
       -> patch preview -> policy-guarded apply/revert
```

新增工具包括 `analyze_changes`、`get_candidate_detail`、`preview_document_patch`、`review_candidate`、`apply_document_patch`、`revert_document_patch`、`run_agent`、`set_runtime_strategy` 和 `doctor`。MCP 仍不暴露任意 Shell、任意文件写入或删除。

项目级 Skills：

- `cyclaw-project-knowledge`：日常变化后的知识维护。
- `cyclaw-session-close`：长任务结束和交接收尾。
- `cyclaw-doc-audit`：发布和合并前的文档漂移审计。

Hook Pipeline：

- `session-start`：只读展示待处理候选、草稿和建议。
- `session-stop`：分析当前工作区并生成知识候选，不绕过策略写文档。
- `pre-commit`：只分析 staged 文件，默认提示；严格模式仅按本轮相关高置信度候选阻止提交。
- Git Hook 支持普通仓库和 worktree；已有非 cyClaw Hook 默认拒绝覆盖，卸载只删除带管理标记的 Hook。

详细配置和后续优先级见 `docs/cyclaw-agent-integrations-v0.1.md`。

## 12. 项目记忆与知识治理（2026-07-19）

产品定位从“自动生成项目文档”调整为“Coding Agent 的项目记忆、事实治理与上下文控制层”。文档只是项目知识的一种输出形式。

Document Patch 现已支持：

```text
create
update
merge
supersede
delete
```

实现约束：

- 使用 Markdown 标题结构定位章节，不对整份文档做无边界字符串替换。
- 同标题候选默认更新原章节，减少重复追加。
- 合并要求多个唯一章节，拒绝互相嵌套的范围。
- supersede 保留历史并记录取代关系。
- delete 支持章节和整文档，原始内容可撤销恢复。
- 应用和撤销均检查文档是否被并发修改。
- CLI、MCP、VS Code 和 IDEA 均展示操作类型。

任务记忆协议已经落地：`begin_task`、`get_task_context`、`record_decision`、`record_failed_approach`、`checkpoint_task`、`reconcile_project_knowledge` 和 `close_task`。

## 13. 任务生命周期与 Fact Ledger（2026-07-19）

任务数据按项目持久化：

```text
.cyclaw/tasks/*.json
.cyclaw/active-task
.cyclaw/memory/facts.jsonl
.cyclaw/reconciliation/*.json
```

任务协议覆盖：

```text
begin -> context -> decision/failure/checkpoint -> reconcile -> close
```

- `begin_task` 创建任务，并立即返回首个上下文包。
- 决策与失败方案在记录时同步进入结构化 Fact Ledger，关闭任务后仍可跨会话召回。
- `checkpoint_task` 保存长任务阶段状态、相关文件和后续上下文。
- `close_task` 默认执行一次知识对账，并清除活动任务指针。
- 任务、事实和对账均写入事件日志，供 IDE 与 Agent 查看。

上下文编译遵守显式 Token 预算：约 60% 优先分配给结构化事实，其余用于文档检索结果和待处理知识候选。每条事实包含来源、相关性分数和召回原因；结果返回 `estimated_tokens`、`budget_tokens` 和 `truncated`，避免全仓库上下文注入。

知识对账当前发现三类问题：

| 问题 | 推荐治理操作 |
| --- | --- |
| duplicate | merge |
| conflict | supersede |
| stale | delete |

对账报告是确定性建议，不会自动修改 Fact 状态。冲突检测属于启发式能力，必须由 Codex 或开发者结合代码证据审阅。

CLI 评测入口：

```powershell
.\scripts\evaluate-task-memory.ps1
```

评测覆盖跨任务决策召回、失败方案召回、上下文预算和重复事实识别。
