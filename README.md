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
- 使用外部模型前，确认发送的上下文符合组织数据政策。
- 自动文档写入默认关闭，所有写入保留 Patch、事件和撤销能力。

完整安全边界和漏洞报告方式见 [SECURITY.md](SECURITY.md)，本地数据与模型 Provider 行为见 [PRIVACY.md](PRIVACY.md)，发布流程见 [发布指南](docs/release-guide.md)。

## 开发命令

```bash
cargo test
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- status
cargo run -p cyclaw-cli -- diff
cargo run -p cyclaw-cli -- watch
cargo run -p cyclaw-cli -- inbox generate
cargo run -p cyclaw-cli -- inbox list
cargo run -p cyclaw-cli -- draft generate --include-pending
cargo run -p cyclaw-cli -- draft list
cargo run -p cyclaw-cli -- index
cargo run -p cyclaw-cli -- search "项目"
cargo run -p cyclaw-cli -- mcp
cargo run -p cyclaw-cli -- model list
cargo run -p cyclaw-cli -- agent run --once --no-model
cargo run -p cyclaw-cli -- policy show
cargo run -p cyclaw-cli -- events list
cargo run -p cyclaw-cli -- hooks run session-stop
cargo run -p cyclaw-cli -- hooks install-git
```

## CLI 完整验证

当前 CLI/Core 的完整本地流程：

```bash
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- watch --once
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

VS Code 的 `cyClaw: 管理高级权限` 展示并控制项目实际能力：本地知识维护、文档写入、模型 API 与联网、自动管理文档、Shell 自动化和源码写入。权限提升时需要确认，并持久化到当前项目 `.cyclaw/config.yaml`。其中“自动管理文档”是独立的高风险权限，开启后 cyClaw Watch 会在发现代码变更时自动生成并写入 `docs/` 文档，不再等待人工确认；它必须与“文档写入”同时开启，默认关闭。

插件的“运行概览”会直接显示当前项目是否发现变更、待处理知识、待应用草稿和自动文档状态。默认模式下，Watch 只收集知识并生成草稿；开启自动管理文档后，概览和 CLI 输出会显示本次自动应用的文档数量，实际修改仍可从 Git diff 和事件日志追溯。

Watch 使用文件内容快照计算相对上一次事件的增量变化；同一个已修改文件继续编辑时仍会触发，但本次分析只包含真正发生变化的文件。CLI 会分别输出分析候选、实际新增、已存在和待处理总数。

常驻 Watch 已升级为操作系统文件事件驱动，不再每 3 秒轮询。文件事件经过短时间合并后，再由 Git 内容快照确认真实变化；空闲时不会输出“未发现新的项目变更”。VS Code 插件默认不自动弹出日志面板，日志可从“查看运行日志”按需打开。

为对齐 Claude Code 和 Codex 的安全边界，插件只在受信任工作区执行本地 CLI；内容快照不跟随符号链接，超过 8MB 的文件使用轻量签名，事件通道有固定容量，避免大型项目和事件风暴造成不必要的资源占用。

VS Code 插件提供观察、审阅、智能审阅和自动文档四种运行策略，并展示最近活动。运行策略是一组常用权限预设；高级权限用于逐项自定义，组合不属于预设时界面显示“自定义权限”。候选可以打开证据详情并一键生成草稿，草稿可以直接查看 Diff，应用后可以撤销。已配置活动模型且授予模型与联网权限时，“运行 Agent”会默认执行模型审查。

候选包含 `confidence`、`reviewed_by_model`、`model_recommendation` 和 `model_rationale`。本地规则初始置信度为高 `90`、中 `75`、低 `55`；模型以结构化 JSON 重新审查并写回结论。自动文档默认阈值为 `90`，且在启用模型能力时只自动应用模型推荐 `keep`、达到阈值并由本轮新生成的草稿。

设置自动写入阈值：

```powershell
cyclaw policy set-auto-threshold 90
```

CLI 开启方式：

```powershell
cyclaw policy enable-auto-docs
```

关闭自动管理文档：

```powershell
cyclaw policy set allow_auto_apply_docs --enabled=false
```

Agent Runtime：

```powershell
cyclaw agent run --once --no-model
cyclaw agent run --once --provider deepseek
```

`--no-model` 只运行本地状态收集、变更分析和候选/草稿统计。指定模型 Provider 后，Agent 会调用用户配置的大模型生成一次项目知识处理建议，并把运行记录写入 `.cyclaw/agent-runs/`。

Agent 运行记录管理：

```powershell
cargo run -p cyclaw-cli -- agent runs list --limit 20
cargo run -p cyclaw-cli -- agent runs clean --keep 20
```

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
- `run_agent`、`set_runtime_strategy`：运行 Agent 和切换当前项目策略。
- `doctor`：检查 Git、初始化、权限、候选、活动模型和文档写入状态。
- `begin_task`、`get_active_task`、`get_task_context`：建立任务边界，并按 Token 预算编译当前任务所需上下文。
- `record_decision`、`record_failed_approach`、`checkpoint_task`：在工作过程中沉淀决策、失败路径和检查点。
- `reconcile_project_knowledge`、`close_task`：发现重复、冲突和失效事实，并完成任务收尾。
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
- 通过 CLI 执行 `init`、`scan`、`watch --once`。
- 支持启动和停止 `cyclaw watch` 常驻监听。
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
- 通过 CLI 执行 `init`、`scan`、`watch --once`。
- 支持启动和停止 `cyclaw watch`。
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
