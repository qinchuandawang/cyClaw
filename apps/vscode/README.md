# cyClaw VS Code / Cursor 插件

这是 cyClaw 的 VS Code / Cursor 编辑器集成层。插件不复制 Rust Core 逻辑，只通过 `cyclaw mcp` 和 CLI 命令读取项目知识、触发扫描和启动监听。

当前发布的 VSIX 内置 Windows x64 CLI。Linux 和 macOS 用户可从源码运行扩展，并通过 `cyclaw.command` 配置本机编译的 CLI 路径。

## 当前能力

- 在 Activity Bar 提供 `cyClaw` 项目知识视图。
- 以 Webview 概览作为首屏，集中展示品牌、运行策略、监听状态、候选/草稿统计、快捷操作、待处理内容和最近活动。
- 概览自动适配 VS Code 深浅主题和窄侧边栏，原树形视图保留为“知识与权限”详细操作区。
- 在“运行概览”中显示变更状态、候选知识、文档草稿和自动管理状态，并提供立即检查和运行 Agent 的入口。
- 在“最近活动”中按时间展示变更检测、候选生成、草稿应用、模型调用和权限变化。
- 点击候选打开证据详情，可直接接受并生成草稿、打开来源或忽略。
- 点击文档草稿打开修改前后 Diff；应用后可立即撤销。
- 文档草稿支持新增、更新、合并、取代和删除五种知识治理操作，列表和 Diff 标题会显示操作类型。
- 首屏任务中心展示当前任务、目标、决策、失败方案、检查点、上下文 Token 和相关项目事实。
- 支持开始任务、记录决策、执行知识对账和关闭任务；最近对账会分别显示重复、冲突和失效数量。
- 提供观察、审阅、智能审阅和自动文档四种运行策略。
- 候选显示规则或模型置信度；模型审查结果会写回候选详情。
- 支持批量接受置信度不低于 80 的候选，以及批量忽略低于 60 或模型建议忽略的候选。
- 读取 `get_project_status`、`list_pending_knowledge`、`list_document_patches`。
- 执行 `cyclaw init`、`cyclaw scan`、`cyclaw watch --once`。
- 启动或停止 `cyclaw watch` 常驻进程。
- Watch 使用操作系统文件事件唤醒，再通过 Git 内容快照去重；空闲时不轮询、不打印日志。
- Output 面板默认不自动弹出，可通过“查看运行日志”按需打开。
- 未受信任的 VS Code 工作区不会自动初始化项目、启动 Watch、调用 MCP 或执行 CLI。
- 在候选知识上右键执行接受或忽略。
- 在文档草稿上右键执行应用。

## 开发运行

```powershell
cd apps/vscode
pnpm install
pnpm run compile
```

在 VS Code 中打开仓库后，进入 Run and Debug，选择 Extension Development Host 运行即可。

## 安装后使用

Windows VSIX 通过仓库根目录的打包脚本生成：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-vscode.ps1
```

当前开发包：

```text
apps/vscode/cyclaw-vscode-0.1.6.vsix
```

该脚本会构建 Release 版 `cyclaw.exe` 并放入 VSIX。安装一次插件后，打开任意单文件夹项目时插件会自动激活，首次打开时初始化 `.cyclaw`，随后启动该项目自己的 Watch。

项目数据始终写在：

```text
<项目根目录>/.cyclaw/
```

因此切换到其他项目不会读取或修改前一个项目的知识、草稿、索引或事件日志。

任务记忆数据位于 `.cyclaw/tasks/`、`.cyclaw/memory/` 和 `.cyclaw/reconciliation/`。插件通过本地 MCP 读取这些结构化数据，不会将整个项目内容发送给模型。

当前版本以单文件夹工作区为单位运行。多根工作区会使用第一个工作区文件夹；需要彼此独立的项目时，请分别用 VS Code 窗口打开。

## 模型与权限

命令面板中提供以下项目级命令：

- `cyClaw: 配置模型 Provider`：输入 Provider 名称、OpenAI-compatible API 地址、模型名和 API Key。API Key 只保存到 VS Code SecretStorage；项目内仅记录环境变量名。
- `cyClaw: 切换活动模型`：在当前项目已配置的 Provider 间切换，不会影响其他项目。
- `cyClaw: 管理高级权限`：逐项控制本地知识维护、文档写入、模型 API 与联网、自动管理文档、Shell 自动化和源码写入。手动组合不属于预设时显示为“自定义权限”。
- `cyClaw: 选择运行策略`：自动配置一组常用底层权限。智能审阅策略会在模型已配置且授权时让“运行 Agent”自动调用活动模型。

运行策略与高级权限的关系：运行策略是预设，高级权限是底层开关。选择策略会覆盖相关权限；之后手动调整高级权限不会修改策略名称，而是根据实际组合重新识别为对应策略或“自定义权限”。

四种模式：

- 观察模式：只检测和收集，不调用模型，不写项目文档。
- 审阅模式：生成候选和草稿，由用户确认后写入。
- 智能审阅：调用活动模型审查，文档仍需确认。
- 自动文档：调用模型并自动写入 `docs/`，属于高风险模式。

启用自动文档模式时需要设置自动写入最低置信度，默认建议 `90`。如果启用了模型能力，候选必须经过模型结构化审查、推荐为 `keep` 且达到阈值后才会自动写入。

“自动管理文档”是独立的高风险权限，含义是：发现代码变更后，自动生成文档草稿并直接写入 `docs/`，不再等待人工确认。该权限默认关闭，且只有同时开启“文档写入”后才会生效。未开启时，cyClaw 仍会自动收集有价值的信息并生成候选知识、文档草稿，用户可以在插件中逐项查看和应用。

也可以在 CLI 中开启：

```powershell
cyclaw policy enable-auto-docs
```

模型配置、权限策略和知识资产均为项目级数据，分别保存在当前项目的 `.cyclaw/` 下；API Key 是本机 VS Code 用户密钥，不写入 `.cyclaw`、Git 或 VSIX。

开发 cyClaw 源码时可启用：

```json
{
  "cyclaw.useCargoRun": true
}
```

这会用 `cargo run -p cyclaw-cli -- ...` 调用当前仓库里的 CLI。发布安装后可以把它改成：

```json
{
  "cyclaw.useCargoRun": false,
  "cyclaw.command": ""
}
```

空字符串表示使用 VSIX 内置的 Windows x64 CLI。也可以填写外部 CLI 的绝对路径覆盖内置版本。

## Codex / Claude Code

VS Code 插件负责项目打开期间的事件驱动 Watch；Codex 或 Claude Code 通过 MCP 和项目级 Skills 完成候选审阅、文档草稿、应用与撤销。不要再为每次文件写入配置重复 Hook。

安装 Skills：

```powershell
.\scripts\install-agent-skills.ps1 -Target Both -ProjectPath "D:\path\to\project"
```

Claude Code 生命周期 Hook、Codex 项目指令和 Git `pre-commit` 配置见 `docs/cyclaw-agent-integrations-v0.1.md`。
