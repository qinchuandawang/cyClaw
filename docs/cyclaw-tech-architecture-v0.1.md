# cyClaw 技术栈与架构选择 v0.2

日期：2026-07-04

## 1. 结论先行

重新核对 Claude Code、Codex、Gemini CLI、Aider、OpenClaw 等产品后，cyClaw 的技术路线建议调整为：

```text
Rust Core First
Tauri + React Desktop
Rust Agent Core
Rust CLI as Debug Entry
Markdown + SQLite + JSONL Storage
Git Diff + Rule Engine + LLM Hybrid Analysis
Patch-based Document Update
Local Event Bus + Hook Pipeline
OpenAI-compatible Model Gateway
MCP Server as Integration Layer
```

一句话结论：

> cyClaw 的核心引擎应该用 Rust，界面和交互层用 TypeScript / React。

这不是因为 Rust 热门，而是因为 cyClaw 的核心问题更像“本地可信知识引擎”，不是“网页聊天应用”。

需要明确的是：

> CLI 不是 cyClaw 的最终用户形态。CLI 是核心能力的调试入口、自动化入口和 IDE/桌面端复用入口。真正的使用体验应该是本地 Agent 常驻监听项目变化，桌面端或 IDE 负责展示提醒、知识收件箱和确认动作。

## 2. 为什么重新调整

上一版建议使用 `TypeScript + Node.js Core`，理由是迭代快、生态丰富、AI 编码友好。

但重新检查现有优秀 AI 编码工具后，需要修正判断：

- OpenAI Codex 官方仓库已经明显转向 Rust，仓库语言统计显示 Rust 占绝大多数，并且包含 `codex-rs` 目录。
- OpenAI 曾在 Codex 讨论区说明过从 TypeScript 实现转向 Rust native 的方向。
- Claude Code 官方文档目前推荐 Native Install、Homebrew、WinGet 等原生安装方式，npm 安装已不是推荐方式。
- Claude Code 的 npm 包文档说明其下载原生二进制，运行时不依赖本机 Node.js。
- Claude Code 是否已经迁移到 Rust，官方公开资料不能确认；但它从“Node/npm 工具”向“原生二进制分发”的趋势是明确的。

这说明一件事：

> 成熟 AI Coding 工具正在把本地执行、权限边界、分发体验、跨平台稳定性放到更高优先级。

cyClaw 的定位是项目知识维护层，长期会涉及：

- 文件系统扫描。
- Git diff 分析。
- 文档 patch。
- 本地索引。
- Git Hook。
- 后台监听。
- 权限控制。
- 本地命令调用。
- 桌面分发。
- MCP Server。

这些都更适合一个可靠的 native core，而不是完全依赖 Node.js 进程。

## 3. 从现有产品借鉴什么

### 3.1 借鉴 Codex：核心能力 native 化

Codex 的关键启发不是“用 Rust 就高级”，而是：

```text
本地执行型 Agent 的核心层应该是稳定、可分发、可控、低依赖的。
```

对 cyClaw 的启发：

- CLI 应该是单二进制或低依赖分发。
- 本地权限控制应该在 core 层实现。
- Git、文件、patch、SQLite、事件日志不应该依赖脆弱的脚本胶水。
- 后续桌面端、Git Hook、MCP 都应该复用同一个 core。

### 3.2 借鉴 Claude Code：事件、记忆、扩展机制

Claude Code 值得借鉴的是：

- 项目级说明文件。
- Hooks。
- Skills / Plugins。
- Subagents。
- MCP。
- 权限和设置体系。
- 原生安装和自动更新。

对 cyClaw 的启发：

```text
知识维护不能依赖模型自觉，必须嵌入开发事件流。
```

cyClaw 应该有自己的事件：

- Git diff detected。
- Dependency changed。
- API surface changed。
- Schema changed。
- Config changed。
- Task finished。
- Knowledge candidate created。
- Document patch generated。
- Pre-commit check started。

### 3.3 借鉴 Gemini CLI：TypeScript 的优势在交互和生态

Gemini CLI 仍然体现了 TypeScript 的优势：

- 社区贡献门槛低。
- 前端和终端 UI 生态成熟。
- 和 Web/VS Code 生态连接自然。
- 产品迭代速度快。

对 cyClaw 的启发：

```text
TypeScript 适合 UI、插件配置、提示词模板和开发者生态，不一定适合承担本地可信核心。
```

### 3.4 借鉴 Aider：Python 适合模型和快速实验

Aider 证明 Python 很适合：

- LLM 编排。
- 快速验证想法。
- 文本处理。
- 原型实验。

但 cyClaw 不建议用 Python 做主工程：

- 桌面分发成本更高。
- Windows 用户环境差异更大。
- 后台守护、文件监听、Git Hook 分发不如 Rust 直接。
- 难以形成轻量单二进制体验。

Python 可以用于实验脚本，不作为主运行时。

## 4. 语言对比

### 4.1 Rust

适合 cyClaw 的地方：

- 单二进制分发，适合 CLI 和后台服务。
- 文件系统、Git、SQLite、patch、索引等本地能力稳定。
- 内存安全，适合处理用户项目文件和权限边界。
- Tauri 原生后端就是 Rust。
- 性能好，适合大项目扫描、增量索引和长时间运行。
- 依赖供应链相比 npm 更可控。

不足：

- UI 开发不如 Web 技术高效。
- LLM 应用层迭代比 TypeScript / Python 慢。
- 开发门槛更高。
- 插件生态不适合直接暴露给普通开发者。

结论：

> Rust 适合作为 cyClaw 的核心引擎、CLI、Git Hook、文件扫描、索引、权限和 patch 层。

### 4.2 TypeScript / Node.js

适合 cyClaw 的地方：

- React / Web UI 生态成熟。
- AI 编码时代，模型对 TypeScript 支持很好。
- 编写配置界面、插件管理、提示词模板很高效。
- 和 VS Code、Cursor、MCP、Web 生态连接自然。
- 适合快速做桌面前端和管理面板。

不足：

- Node runtime 分发和版本管理会增加负担。
- 文件监听、权限边界、长期后台服务更容易遇到平台差异。
- npm 供应链风险高。
- 做本地可信执行核心时，需要额外约束。

结论：

> TypeScript 适合 cyClaw 的前端、插件配置、提示词模板、开发工具链，不适合作为核心执行引擎。

### 4.3 Go

适合 cyClaw 的地方：

- 单二进制分发简单。
- 并发和后台服务稳定。
- CLI 开发体验好。
- 开发效率比 Rust 更高。

不足：

- Tauri 生态不如 Rust 自然。
- AST / Tree-sitter / patch / 嵌入式索引生态整体不如 Rust 贴合。
- 类型表达和安全边界不如 Rust 严密。

结论：

> Go 是 Rust 的务实替代方案，但如果已经选择 Tauri，Rust 更顺。

### 4.4 Python

适合 cyClaw 的地方：

- LLM/RAG/数据处理生态强。
- 原型速度快。
- 文本处理和脚本实验方便。

不足：

- 桌面和 CLI 分发复杂。
- 用户环境差异明显。
- 长期本地守护和跨平台体验不够理想。
- 不适合作为安全边界核心。

结论：

> Python 可用于实验和离线分析脚本，不作为主工程语言。

### 4.5 Kotlin / JVM

适合 cyClaw 的地方：

- 适合 JetBrains 插件。
- 企业项目生态强。
- 类型系统和并发能力不错。

不足：

- CLI 和轻量桌面分发不够自然。
- 冷启动和运行时体积偏重。
- 与 Tauri / Web UI 路线不匹配。

结论：

> 暂不适合作为 cyClaw 主栈。

## 5. 最终架构

推荐架构：

```text
apps/
  desktop/                 # Tauri + React
  cli/                     # Rust CLI

crates/
  cyclaw-core/             # 核心编排
  cyclaw-scanner/          # 项目扫描
  cyclaw-change-radar/     # 变更雷达
  cyclaw-knowledge/        # 知识捕获
  cyclaw-docs/             # 文档维护和 patch
  cyclaw-retrieval/        # 检索与索引
  cyclaw-policy/           # 权限与策略
  cyclaw-model/            # 模型网关
  cyclaw-events/           # 事件总线
  cyclaw-mcp/              # MCP Server

packages/
  ui/                      # React UI 组件
  prompts/                 # 提示词模板
  schemas/                 # JSON Schema / TypeScript 类型生成
```

运行时关系：

```text
React UI
  |
Tauri Commands
  |
Rust Core Engine
  |
  |-- Project Scanner
  |-- Change Radar
  |-- Knowledge Capturer
  |-- Doc Maintainer
  |-- Retrieval Engine
  |-- Policy Engine
  |-- Model Gateway
  |-- Event Bus
  |-- MCP Server
  |
Markdown + SQLite + JSONL
```

## 6. 模块选择

### 6.1 CLI

语言：

```text
Rust
```

建议命令：

```text
cyclaw init
cyclaw scan
cyclaw diff
cyclaw inbox
cyclaw draft
cyclaw apply
cyclaw search
cyclaw precommit
cyclaw mcp
```

原因：

- Git Hook 可以直接调用。
- Codex / Claude Code 可以直接调用。
- 不依赖 Node。
- 后续可单二进制发布。

### 6.2 桌面端

技术：

```text
Tauri + React + TypeScript
```

原因：

- Tauri 与 Rust core 自然集成。
- React 适合知识面板、diff 预览、收件箱、检索 UI。
- 桌面端不承担核心逻辑，只做交互和确认。

### 6.3 本地存储

技术：

```text
Markdown + SQLite + JSONL
```

职责：

- Markdown：用户可读、可提交到 Git 的知识资产。
- SQLite：索引、状态、来源、关联关系。
- JSONL：事件日志、模型调用摘要、审计轨迹。

### 6.4 检索

第一版：

```text
SQLite FTS5 + 结构化元数据
```

第二阶段：

```text
可选向量索引
```

原因：

- 项目知识检索首先需要来源可靠。
- 关键词、文件路径、符号、文档标题、变更类型比纯向量更可控。
- 向量检索作为语义增强，不作为唯一检索路径。

### 6.5 代码与变更分析

技术路线：

```text
Git diff -> 文件类型规则 -> Tree-sitter -> LLM 语义判断
```

原则：

- 能用规则判断的，不交给模型。
- 能用 AST 判断的，不靠文本猜测。
- 模型用于语义摘要、重要性评分、文档草稿。

### 6.6 文档写入

技术路线：

```text
Patch-based Document Update
```

流程：

```text
读取目标文档
生成结构化更新草稿
生成 diff
用户确认
应用 patch
记录来源
```

禁止第一版让模型直接覆盖整篇文档。

### 6.7 模型网关

技术：

```text
Rust reqwest + Provider Adapter
```

首批适配：

- OpenAI-compatible API。
- Anthropic。
- DeepSeek。
- Qwen。
- Kimi。
- GLM。
- Ollama。

要求：

- 结构化输出。
- Schema 校验。
- 调用摘要日志。
- 可配置是否允许上传代码片段。

### 6.8 插件与技能

第一版不做可执行插件市场。

推荐：

```text
Markdown Skill + YAML Rule + Built-in Rust Executor
```

原因：

- 保持安全边界。
- 用户可以读懂。
- 方便被 Git 管理。
- 后续再开放 WASM 插件或 JS 沙箱。

## 7. Rust 生态建议

可选库方向：

| 能力 | Rust 方向 |
| --- | --- |
| CLI | `clap` |
| 异步运行时 | `tokio` |
| HTTP | `reqwest` |
| 序列化 | `serde` |
| 配置 | `figment` 或 `config` |
| SQLite | `rusqlite` 或 `sqlx` |
| 文件监听 | `notify` |
| Git | `gix` 或调用 `git` CLI |
| diff/patch | `similar`、`patch` 或自研受控 patch |
| 搜索忽略规则 | `ignore` |
| Markdown | `pulldown-cmark`、`markdown` |
| Tree-sitter | `tree-sitter` |
| 错误处理 | `anyhow`、`thiserror` |
| 日志 | `tracing` |
| Tauri | `tauri` |

第一版建议优先调用 `git` CLI，而不是深度绑定 libgit2：

- Git CLI 行为更贴近用户实际环境。
- 能复用用户已有 Git 配置。
- MVP 更稳。

后续如果需要高性能增量分析，再引入 `gix`。

## 8. 为什么不是纯 Rust

不建议把所有东西都写成 Rust。

原因：

- UI 开发效率不如 React。
- 管理界面、交互状态、diff 展示、设置页用 Web 技术更合适。
- 提示词模板、技能说明、规则配置应该保持文本化和可编辑。
- 未来 VS Code / Cursor 插件天然需要 TypeScript。

因此更合理的是：

```text
Rust 负责可信执行核心
TypeScript 负责用户界面和开发者生态
Markdown/YAML 负责可配置知识规则
```

## 9. 为什么不是纯 TypeScript

不建议继续采用纯 TypeScript / Node.js core。

原因：

- cyClaw 不是普通 Web 应用。
- 核心能力是本地扫描、权限控制、Git Hook、patch、索引和长期运行。
- Node 运行时和 npm 依赖会增加分发与安全成本。
- 成熟工具正在向 native 分发靠拢。
- Rust core 更适合作为后续 CLI、桌面端、MCP Server、Git Hook 的共同基础。

TypeScript 仍然重要，但它不应该承担核心执行边界。

## 10. 安全与权限设计

第一版默认策略：

```text
读：当前项目目录
写：docs/ 与 .cyclaw/
命令：只允许 git status、git diff、git log 等只读命令
联网：默认关闭，模型调用需显式配置
文档写入：必须生成 diff，用户确认后应用
```

风险等级：

| 等级 | 示例 |
| --- | --- |
| 低 | 读取 Git diff、扫描 docs |
| 中 | 生成文档 patch、写入 `.cyclaw/` |
| 高 | 修改 `docs/`、上传代码片段到模型 |
| 禁止默认执行 | 删除文件、运行任意 shell、修改代码 |

cyClaw 的定位不是改代码，因此默认不需要修改源码文件。

## 11. MVP 实施顺序

### 阶段 1：Rust CLI + 项目扫描

目标：

- `cyclaw init`
- `cyclaw scan`
- 生成 `.cyclaw/project-profile.json`
- 识别项目语言、依赖文件、文档目录、Git 状态。

### 阶段 2：变更雷达

目标：

- `cyclaw diff`
- 读取 Git diff。
- 识别 API、Schema、依赖、配置、环境变量变化。
- 生成 `.cyclaw/runs/{id}/change-analysis.json`。

### 阶段 3：知识收件箱

目标：

- `cyclaw inbox`
- 从 diff 和任务总结中生成候选知识。
- 给出重要性评分。
- 写入 `.cyclaw/knowledge-inbox.jsonl`。

### 阶段 4：文档草稿和 patch

目标：

- `cyclaw draft`
- 生成 docs 更新草稿。
- 展示 diff。
- `cyclaw apply` 用户确认后写入。

### 阶段 5：检索

目标：

- `cyclaw index`
- `cyclaw search`
- SQLite FTS5 检索 docs、`.cyclaw/memory.md`、知识收件箱、历史任务。

### 阶段 6：Tauri 桌面面板

目标：

- 展示知识资产目录。
- 展示变更雷达。
- 展示知识收件箱。
- 展示文档 diff。
- 支持人工确认。

### 阶段 7：MCP Server

目标：

- `cyclaw mcp`
- 让 Codex、Claude Code、Cursor 等工具查询 cyClaw 项目记忆。
- 让 AI 编码工具在改代码前知道项目规则和历史坑点。

## 12. 最终推荐

当前最适合 cyClaw 的选择是：

```text
主语言：Rust
桌面 UI：Tauri + React + TypeScript
CLI：Rust
核心存储：Markdown + SQLite + JSONL
检索：SQLite FTS5 优先，向量检索后置
分析：Git diff + 规则引擎 + Tree-sitter + LLM
写入：Patch-based Document Update
扩展：MCP Server + Markdown/YAML Skills
```

核心原则：

> 用 Rust 承担本地可信核心，用 TypeScript 承担交互效率，用 Markdown 和 SQLite 承担长期项目记忆。

这个选择未必最常见，也不是为了追热点。它更符合 cyClaw 的产品本质：

> cyClaw 要成为项目知识的守门人，而守门人的核心层必须稳定、可审计、可分发、可长期运行。

## 13. 参考资料

- Claude Code 官方仓库：https://github.com/anthropics/claude-code
- Claude Code 安装文档：https://code.claude.com/docs/en/setup
- OpenAI Codex 官方仓库：https://github.com/openai/codex
- Codex CLI is Going Native 讨论：https://github.com/openai/codex/discussions/1174
- Gemini CLI 官方仓库：https://github.com/google-gemini/gemini-cli
- Aider 官方仓库：https://github.com/Aider-AI/aider
- OpenClaw 官方仓库：https://github.com/openclaw/openclaw
