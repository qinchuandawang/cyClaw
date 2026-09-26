# cyClaw

> 面向 Coding Agent 的项目记忆、事实治理与上下文控制层。

cyClaw 不替代 Codex、Claude Code 或 IDE 的编码能力。它在任务开始前召回项目约束，在工作过程中沉淀决策和失败路径，在任务结束后识别重复、冲突和失效知识，避免项目文档随着 AI 编码不断熵增。

## 为什么使用

- **跨会话项目记忆**：记录决策、限制、失败方案和检查点，并在后续任务中按相关性召回。
- **知识治理而非无止境追加**：统一支持 `create`、`update`、`merge`、`supersede`、`delete` 五种操作。
- **受预算的上下文**：优先提供有来源、有原因的结构化事实，而非把整个仓库塞给模型。
- **本地优先、权限明确**：数据默认保存在项目 `.cyclaw/`，模型联网、文档写入和自动化均需单独授权。
- **原生接入 Coding Agent**：通过本地 stdio MCP、项目 Skills、Git Hook、VS Code 和 IDEA 插件接入现有工作流。

## 支持状态

当前为 `v0.x` 预稳定阶段。CLI 可从源码在 Windows、Linux、macOS 构建；官方 VSIX 当前仅内置 Windows x64 CLI。MCP 协议和 `.cyclaw/` 存储格式仍可能在小版本中演进。

| 组件 | 当前状态 |
| --- | --- |
| Rust CLI / 本地 stdio MCP | Windows、Linux、macOS 源码构建与 Release CLI |
| VS Code / Cursor | Windows x64 VSIX；其他平台可从源码运行扩展并配置外部 CLI |
| IntelliJ IDEA | Java 17，Community 2023.3 及兼容版本 |
| 模型 Provider | OpenAI-compatible API，用户自带地址和 API Key |

## 快速开始

### 从源码运行 CLI

```powershell
git clone https://gitee.com/zhu-chengyuu/cy-claw.git
cd cyclaw
cargo run -p cyclaw-cli -- init --path <project-path>
cargo run -p cyclaw-cli -- scan --path <project-path>
cargo run -p cyclaw-cli -- status --path <project-path>
```

在目标项目中建立并结束一次任务：

```powershell
cyclaw task begin "接口改造" --objective "实现变更并保持兼容" --path <project-path>
cyclaw task decision "关键设计决策" --rationale "决策依据" --path <project-path>
cyclaw task reconcile --path <project-path>
cyclaw task close "任务完成" --path <project-path>
```

### 接入 Codex

构建并注册本地 stdio MCP：

```powershell
.\scripts\install-codex-mcp.ps1
```

注册后新建 Codex 会话。cyClaw 会以 Codex 当前项目目录为边界，并把每个项目的知识隔离在各自的 `.cyclaw/` 下。完整接入方式见 [Agent 集成方案](docs/cyclaw-agent-integrations-v0.1.md)。

## 数据与安全

- `.cyclaw/` 包含项目画像、任务、结构化事实、索引、候选、文档草稿和审计事件；默认不应提交到 Git。
- CLI 只在配置中保存 API Key 环境变量名；VS Code API Key 保存在本机 SecretStorage。
- 模型响应缓存默认关闭；需要缓存时必须在项目策略中显式开启。
- 使用外部模型前，确认发送的上下文符合组织数据政策。
- cyClaw 不会依据路径规则候选自动写入文档；所有写入保留 Patch、事件和撤销能力。

完整安全边界和漏洞报告方式见 [SECURITY.md](SECURITY.md)，本地数据与模型 Provider 行为见 [PRIVACY.md](PRIVACY.md)，发布流程见 [发布指南](docs/release-guide.md)。

## 开发命令

```bash
cargo test
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- status
cargo run -p cyclaw-cli -- diff
cargo run -p cyclaw-cli -- observer run
cargo run -p cyclaw-cli -- inbox generate
cargo run -p cyclaw-cli -- inbox list
cargo run -p cyclaw-cli -- draft generate --include-pending
cargo run -p cyclaw-cli -- draft list
cargo run -p cyclaw-cli -- index
cargo run -p cyclaw-cli -- search "项目"
cargo run -p cyclaw-cli -- mcp
cargo run -p cyclaw-cli -- model list
cargo run -p cyclaw-cli -- observer run --once
cargo run -p cyclaw-cli -- policy show
cargo run -p cyclaw-cli -- events list
cargo run -p cyclaw-cli -- trace list
cargo run -p cyclaw-cli -- trace show <trace-id>
cargo run -p cyclaw-cli -- model calls list
cargo run -p cyclaw-cli -- retry list
cargo run -p cyclaw-cli -- retry run
cargo run -p cyclaw-cli -- hooks run session-stop
cargo run -p cyclaw-cli -- hooks install-git
```

## CLI 完整验证

当前 CLI/Core 的完整本地流程：

```bash
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- observer run --once
cargo run -p cyclaw-cli -- inbox list --pending
cargo run -p cyclaw-cli -- draft generate --include-pending
cargo run -p cyclaw-cli -- draft list
cargo run -p cyclaw-cli -- index
cargo run -p cyclaw-cli -- search "依赖"
cargo run -p cyclaw-cli -- status
```

Windows 端到端 smoke 验证：

```powershell
.\scripts\smoke-cli.ps1
```

这个脚本会创建临时 Git 项目，制造依赖和环境变量变更，并跑通 init、scan、watch、inbox、draft、index、search、status、MCP tools/resources、accept、draft apply 全链路。

插件验证：

```powershell
.\scripts\verify-plugins.ps1
```

严格要求 IDEA 插件也完成 Gradle 构建时：

```powershell
.\scripts\verify-plugins.ps1 -RequireIdea
```

Codex MCP 验证：

```powershell
.\scripts\verify-codex-mcp.ps1
```

把 cyClaw 安装到当前机器的 Codex 桌面应用/CLI：

```powershell
.\scripts\install-codex-mcp.ps1
```

注册结果等价于：

```toml
[mcp_servers.cyclaw]
command = "<path-to-cyclaw>"
args = ["mcp"]
```

不固定 `--path`，cyClaw 使用 Codex 启动 MCP 时的当前工作目录。不同项目的知识、索引、候选和草稿仍分别保存在各自的 `.cyclaw/` 下。注册后需要新建 Codex 会话或重启 Codex，已经打开的会话不会动态增加 MCP 工具。

需要把 MCP 固定到单个项目时，也可以使用：

```toml
[mcp_servers.cyclaw]
command = "<path-to-cyclaw>"
args = ["mcp", "--path", "<project-path>"]
```

模型 Provider 验证：

```powershell
$env:DEEPSEEK_API_KEY = "你的 API Key"
.\scripts\verify-model-provider.ps1
Remove-Item Env:DEEPSEEK_API_KEY
```

当前已验证 DeepSeek OpenAI-compatible 配置：

```powershell
cyclaw model add deepseek `
  --base-url https://api.deepseek.com `
  --model deepseek-v4-flash `
  --api-key-env DEEPSEEK_API_KEY

cyclaw model test deepseek
```

cyClaw 只把 `DEEPSEEK_API_KEY` 这个环境变量名写入 `.cyclaw/model-providers.yaml`，不会把 API Key 写入项目文件。

在 VS Code 插件中可直接执行 `cyClaw: 配置模型 Provider`。插件会要求输入 OpenAI-compatible API 地址、模型名称和 API Key；API Key 仅保存于 VS Code SecretStorage，项目目录仍只保存环境变量名。`cyClaw: 切换活动模型` 会切换当前项目的 Provider，不会影响其他项目。

模型思考模式默认关闭。只有显式传入 `--thinking` 时才会开启：

```powershell
cyclaw model add deepseek `
  --base-url https://api.deepseek.com `
  --model deepseek-v4-flash `
  --api-key-env DEEPSEEK_API_KEY `
  --thinking
```

VS Code 的 `cyClaw: 管理高级权限` 只控制真实能力：本地知识维护、文档 Patch 应用、模型 API 与联网。权限提升会写入当前项目 `.cyclaw/config.yaml`。cyClaw 不提供 Shell 自动化、源码写入或基于候选的自动文档写入。

插件的“运行概览”会显示 Observer 状态、待处理知识和待应用草稿。文档草稿始终需要通过可审计 Patch 明确审批，实际修改可从 Git diff 和事件日志追溯。

Watch 使用文件内容快照计算相对上一次事件的增量变化；同一个已修改文件继续编辑时仍会触发，但本次分析只包含真正发生变化的文件。CLI 会分别输出分析候选、实际新增、已存在和待处理总数。

常驻 Observer 使用操作系统文件事件驱动，不再每 3 秒轮询。文件事件经过短时间合并后，再由 Git 内容快照确认真实变化；空闲期间会周期验证 Fact 证据并执行知识对账。VS Code 插件默认不自动弹出日志面板，日志可从“查看运行日志”按需打开。

为对齐 Claude Code 和 Codex 的安全边界，插件只在受信任工作区执行本地 CLI；内容快照不跟随符号链接，超过 8MB 的文件使用轻量签名，事件通道有固定容量，避免大型项目和事件风暴造成不必要的资源占用。

VS Code 插件提供观察、审阅和智能审阅三种策略，并展示最近活动。Observer 独立处理事件，候选可打开证据详情并生成草稿；草稿可以查看 Diff、应用和撤销。模型审阅是可选的辅助确认，不是事件采集前提。

候选包含 `confidence`、`reviewed_by_model`、`model_recommendation` 和 `model_rationale`。路径分类仅作召回，初始置信度为高 `70`、中 `55`、低 `40`；模型或额外证据可以提高可信度，但不会直接写入项目文档。

CLI 开启方式：

独立 Observer：

```powershell
cyclaw observer run --path .
cyclaw observer run --once --path .
cyclaw observer status --path .
cyclaw observer install --path .
```

Observer 启动时会补偿扫描已有变化，随后独立监听文件/Git/测试报告，并周期验证 Fact 证据和执行知识对账。它不依赖 Codex MCP 调用、任务协议或 IDE 是否打开。

Windows 可使用 `cyclaw observer install --path .` 注册当前用户登录时自动启动的任务，并立即启动 Observer；`cyclaw observer uninstall --path .` 可删除该任务。

Observer 黑盒验证会创建隔离 Git 项目，模拟源码修改、Surefire 失败报告和进程重启：

```powershell
.\scripts\test-observer-e2e.ps1
```

Agent 运行记录管理：

```powershell
cargo run -p cyclaw-cli -- agent runs list --limit 20
cargo run -p cyclaw-cli -- agent runs clean --keep 20
```

## 候选审查双角色（可选）

单次 LLM 审查存在单点偏差：审查者既是提议者又是裁决者。开启双角色后，`agent run` 的模型审查变为两阶段——**审查者**照常给出 keep/ignore 与置信度，**怀疑者**（critic）只针对所有 keep 结论做对抗性复核：

```yaml
# .cyclaw/config.yaml
model_policy:
  review_critic_enabled: true
```

融合规则是确定性且可解释的：`confirm` 保留审查者结论；`downgrade` 取两者中更低的置信度；`reject` 改为 ignore 并取更低置信度。怀疑者只复核 keep 候选（无 keep 时跳过调用，不产生额外成本）；怀疑者调用失败不否决审查者结论，自动降级为单角色结果并在 step 与 trace 中留痕。两次调用均写入 `ModelCalled` 事件（`role` 字段区分）与链路追踪（phase 区分），重试队列语义不变。注意：开启后每次审查为两次模型调用，请相应调整 `daily_token_budget`。

## 可观测性：链路追踪与大模型推理路径

cyClaw 为每次 Agent 运行、命令执行和模型推理保留本地链路数据，用于回答“一次任务发生了什么、一次推理是怎么发生的、一次执行失败意味着什么”：

```text
.cyclaw/traces.jsonl        TraceSpan：trace_id / span_id / parent_span_id 构成的链路树
.cyclaw/model-calls.jsonl   大模型推理明细：tokens、延迟、重试次数、缓存命中、脱敏输入输出预览
.cyclaw/execution-events.jsonl  执行事件（已有），与 trace_id 显式关联
```

Agent run 的 `run_id` 即其 `trace_id`，一次运行形成 `agent_run -> model_review -> model_call` 的完整链路；`cyclaw exec` 沿用外部传入的 `--trace-id` 或自动生成。查询入口：

```powershell
cyclaw trace list --limit 20          # 最近链路索引：span 数、错误数、总耗时
cyclaw trace show <trace-id>          # 还原完整链路 + 推理路径 + 关联执行事件
cyclaw model calls list               # 大模型推理明细，支持 --provider 过滤
```

MCP 侧提供 `list_traces`、`get_trace` 和 `list_model_calls`，Coding Agent 可在失败后直接回放链路。推理明细只保存脱敏后的 240 字符预览，观测写入失败不阻断主流程；完整设计见 [可观测性设计](docs/cyclaw-observability-v0.1.md)。

## 可靠性：失败重试与降级

模型审查等 AI 任务失败时（网络抖动、限流、预算不足、超时），cyClaw 将失败上下文持久化到 `.cyclaw/retry-queue.json`，按指数退避（60s 起、翻倍增长、1 小时封顶）计划异步重试，无需人工值守：

```powershell
cyclaw retry list                  # 查看队列：状态、尝试次数、下次重试时间
cyclaw retry run                   # 立即执行所有到期重试
cyclaw retry abandon <retry-id>    # 确认是逻辑错误时人工放弃
```

重试就是"带失败上下文的又一次完整 run"：候选重新收集、审查重新执行，产物与普通 run 一致（运行记录、推理明细、trace 可查）。重试耗尽（默认 5 次尝试）后任务标记 `degraded` 并写入 `RetryTaskExhausted` 审计事件——**候选知识保持待审状态，模型不写入任何文档**，自动化失败永远不转化为未授权写入。MCP 侧提供 `list_retries`、`run_retries` 和 `abandon_retry`，Coding Agent 可在会话开始时自动恢复上次遗留的失败任务；常驻 Observer 也会在维护周期空闲时自动执行到期重试。

重试参数可按项目覆盖（`.cyclaw/config.yaml`）：

```yaml
model_policy:
  retry_base_delay_seconds: 60
  retry_max_attempts: 5
  retry_max_backoff_seconds: 3600
```

Provider 支持降级链：`cyclaw model add primary ... --fallback backup`，主 Provider 请求失败（网络、HTTP、解析）时自动切换备用 Provider 重试一次，两次调用均写入推理明细（`fallback_from` 标记来源）。完整状态机与设计取舍见 [可靠性设计](docs/cyclaw-reliability-v0.1.md)。

## MCP 工具

当前 `cyclaw mcp` 暴露读取、分析和受控写入工具：

- `get_project_status`：读取当前 cyClaw 状态和建议下一步。
- `get_project_profile`：读取 `.cyclaw/project-profile.json`。
- `search_project_knowledge`：搜索本地知识索引，结果包含来源路径。
- `list_pending_knowledge`：列出待处理候选知识。
- `list_document_patches`：列出待应用文档草稿。
- `analyze_changes`：执行一次变化分析并生成候选知识。
- `get_candidate_detail`、`review_candidate`：查看证据并审阅候选。
- `preview_document_patch`：生成或复用草稿，不修改目标文档。
- `apply_document_patch`、`revert_document_patch`：应用或撤销通过权限检查的文档变更。
- `get_model_usage`：读取当日模型调用次数、Token 用量和日预算。
- `list_traces`、`get_trace`：按 trace_id 聚合或还原完整调用链，含大模型推理路径。
- `list_model_calls`：读取大模型推理明细：Provider、Token、延迟、重试、缓存命中与脱敏预览。
- `list_retries`：读取失败任务重试队列：状态、尝试次数、退避计划与降级原因。
- `run_retries`、`abandon_retry`：立即执行到期重试，或人工放弃并降级为人工处理。
- `cyclaw exec --kind test -- cargo test`：通过统一命令包装器执行测试、构建或其他命令，自动记录退出码、超时、错误摘要、Git commit、session_id 和 trace_id，并将失败送入候选层。
- MCP `begin_task` 可传入 `session_id`；不同窗口的活动任务指针按 session 隔离，项目级 Fact 仍可共享。
- `get_active_task`、`get_task_context`、`checkpoint_task` 和 `close_task` 同样支持 `session_id`，显式 task_id 也会校验任务归属。
- Agent 模型审查会先持久化 pending 状态；调用失败或进程中断后，下次运行复用原 run_id 继续审查，成功后清理恢复状态。
- `run_agent`、`set_runtime_strategy`：兼容性的批处理审阅与策略设置，不承担事件采集职责。
- `doctor`：检查 Git、初始化、权限、候选、活动模型和文档写入状态。
- `begin_task`、`get_active_task`、`get_task_context`：历史兼容的任务上下文读取接口。
- `record_decision`、`record_failed_approach`、`checkpoint_task`：人工补录入口，不要求 Coding Agent 调用。
- `reconcile_project_knowledge`、`close_task`：手动诊断入口；Observer 会自动执行相同的对账能力。
- `list_project_facts`、`list_tasks`、`get_latest_reconciliation`：读取跨会话项目事实、任务历史和最近对账报告。

MCP 不提供任意 Shell、任意文件写入或删除能力。所有写操作复用 Rust Core 的权限、路径、置信度和事件审计约束。

文档 Patch 支持五种知识治理操作：

```text
create / update / merge / supersede / delete
```

未指定操作时，同标题章节优先生成 `update`；只有不存在相关章节时才生成 `create`。详细 CLI 和 MCP 参数见 [知识治理操作](docs/cyclaw-knowledge-operations-v0.1.md)。

当前 resources 覆盖 `.cyclaw/project.md`、`.cyclaw/knowledge-inbox.jsonl`、`.cyclaw/doc-patches/*.json`、`.cyclaw/memory/`、`.cyclaw/tasks/`、`.cyclaw/reconciliation/` 和 `docs/**/*.md`。

### Fact Ledger 治理

结构化事实同样采用 Patch 生命周期，不会由 MCP、CLI 或编辑器直接改写 `facts.jsonl`。先用 `preview_fact_patch` 生成草稿，再用 `apply_fact_patch` 应用；`revert_fact_patch` 仅在应用后的事实仍与草稿指纹一致时恢复，避免覆盖并发修改。`list_fact_patches` 可查看所有审计记录和撤销状态。

支持的事实操作为 `create`、`update`、`merge`、`supersede`、`delete`：合并保留主 Fact 并将重复项标为 `superseded`，取代会保留旧 Fact 并让新 Fact 通过 `supersedes` 建立历史关系，删除只标记为 `deleted`。新事实使用 `FactEvidence` 保存路径、可选符号和行范围、内容哈希、Git 提交、采集/验证时间与证据类型；旧版 `evidence: Vec<String>` 仍可读取。

CLI 示例：

```powershell
cyclaw fact preview --operation delete --target fact_123 --path <project-path>
cyclaw fact list --path <project-path>
cyclaw fact apply fact_patch_123 --path <project-path>
cyclaw fact revert fact_patch_123 --path <project-path>
```

## 任务记忆与上下文

cyClaw 将一次 Coding Agent 工作视为有边界的任务。任务开始时召回相关事实，执行中记录重要决策和失败方案，结束时执行知识对账：

```powershell
cyclaw task begin "退款接口改造" `
  --objective "增加幂等退款并保持旧客户端兼容" `
  --related-file "src/refund.rs"

cyclaw task decision "退款请求必须携带 idempotency_key" `
  --rationale "避免网络重试造成重复退款" `
  --evidence "src/refund.rs"

cyclaw task failure "仅依赖客户端重试次数" `
  --reason "无法覆盖服务端超时后已执行但未返回的情况"

cyclaw task checkpoint "接口与数据模型已完成，待补集成测试"
cyclaw task context --query "退款兼容约束" --budget-tokens 2000
cyclaw task close "退款接口改造完成"
```

结构化数据按项目隔离保存：

```text
.cyclaw/tasks/*.json                 任务记录
.cyclaw/active-task                  当前任务 ID
.cyclaw/memory/facts.jsonl           跨会话 Fact Ledger
.cyclaw/reconciliation/*.json        知识对账报告
```

上下文编译器默认优先将约 60% Token 预算用于结构化事实，其余预算用于相关文档片段和待处理候选。返回结果包含事实来源、相关性、召回原因、估算 Token 和是否截断，避免向 Coding Agent 注入整个项目。

知识对账目前使用确定性和启发式规则发现 `duplicate`、`conflict`、`stale`，分别建议 `merge`、`supersede`、`delete`。对账不会自动修改事实状态或删除文档，必须由 Codex 或开发者审阅后执行治理操作。

## Agent Skills 与 Hooks

仓库提供 `cyclaw-project-knowledge`、`cyclaw-session-close` 和 `cyclaw-doc-audit` 三个 Skills。安装到目标项目：

```powershell
.\scripts\install-agent-skills.ps1 -Target Both -ProjectPath "D:\path\to\project"
```

生命周期和 Git Hook：

```powershell
cyclaw hooks run session-start --path .
cyclaw hooks run session-stop --path .
cyclaw hooks install-git --path .
cyclaw hooks status --path .
```

Git `pre-commit` 默认只提示；设置 `CYCLAW_HOOK_STRICT=1` 后，本轮 staged 变化产生高置信度知识候选时会阻止提交。完整的 Codex、Claude Code、Skills 和 Hook 配置见 [Agent 集成方案](docs/cyclaw-agent-integrations-v0.1.md)。

## VS Code / Cursor 插件

第一版编辑器集成位于：

```text
apps/vscode
apps/idea
```

开发验证：

```powershell
cd apps/vscode
pnpm install
pnpm run check
pnpm run compile
```

当前插件能力：

- 提供 `cyClaw` Activity Bar 入口。
- 展示项目状态、待处理候选知识、待应用文档草稿。
- 通过 `cyclaw mcp` 读取项目知识。
- 通过 CLI 执行 `init`、`scan`、`observer run --once`。
- 支持启动和停止 `cyclaw observer run` 常驻监听。
- 支持在候选知识上执行接受、忽略。
- 支持在文档草稿上执行应用。

Windows 开发测试包会内置 Release 版 CLI。执行：

```powershell
.\scripts\package-vscode.ps1
```

然后安装 `apps/vscode/cyclaw-vscode-0.1.6.vsix`。安装后插件会在打开单文件夹项目时自动激活；首次打开自动初始化和扫描，并对当前项目启动 Watch。每个项目的状态、索引、候选知识、草稿、任务事实和事件都保存在各自的 `<项目根目录>/.cyclaw/` 下，彼此隔离。

当前 VS Code 插件以单文件夹工作区为运行边界。多根工作区会使用第一个文件夹；不同项目需要隔离运行时，请在不同 VS Code 窗口中打开。

## IntelliJ IDEA 插件

第一版 JetBrains 插件工程位于：

```text
apps/idea
```

当前插件能力：

- 提供 `cyClaw` Tool Window。
- 展示项目状态、待处理候选知识、待应用文档草稿。
- 通过 `cyclaw mcp` 读取项目知识。
- 通过 CLI 执行 `init`、`scan`、`observer run --once`。
- 支持启动和停止 `cyclaw observer run`。
- 支持接受、忽略候选知识。
- 支持应用文档草稿。

开发命令：

```powershell
cd apps/idea
.\gradlew.bat buildPlugin
.\gradlew.bat runIde
```

如果当前环境没有 Gradle 且缺少 `apps/idea/gradle/wrapper/gradle-wrapper.jar`，先执行：

```powershell
gradle wrapper --gradle-version 8.7 --distribution-type bin
```
