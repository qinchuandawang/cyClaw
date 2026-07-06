# cyClaw 完成计划 v0.1

日期：2026-07-04

## 1. 目标

cyClaw 的目标不是做一个通用 AI 编码工具，而是做：

> AI Coding 时代的项目知识维护层。

第一版要完成的最小闭环是：

```text
发现变化 -> 捕获知识 -> 生成建议 -> 用户确认 -> 写入文档 -> 支持检索
```

本计划围绕这个闭环制定，优先保证核心工程能力，而不是堆叠聊天、Agent Team、插件市场等复杂功能。

## 2. 最终交付形态

第一阶段完成后，cyClaw 应该具备以下形态：

```text
Rust Agent Core
Rust CLI 调试入口
Rust Core Engine
Tauri + React 桌面知识面板
Markdown + SQLite + JSONL 本地存储
Git Diff 变更雷达
知识收件箱
文档 Patch 更新
本地知识检索
MCP Server
```

说明：

CLI 是工程入口，不是最终用户的主要使用方式。cyClaw 的最终使用形态应该是：

```text
用户打开项目 -> cyClaw Agent 自动启动 -> 自动监听变更 -> 自动生成知识建议 -> 用户在桌面端或 IDE 中确认
```

## 3. 成功标准

MVP 成功标准：

1. 能在任意 Git 项目中运行 `cyclaw init` 初始化。
2. 能扫描项目并生成项目画像。
3. 能读取 Git diff 并识别至少 5 类高价值变化：
   - API 变化
   - 数据模型变化
   - 依赖变化
   - 配置变化
   - 环境变量变化
4. 能生成知识收件箱条目，并给出重要性评分和推荐写入位置。
5. 能生成文档更新草稿和 diff。
6. 用户确认后能写入 `docs/` 或 `.cyclaw/`。
7. 能索引 Markdown 和知识收件箱，并支持本地检索。
8. 能通过桌面面板查看变更雷达、知识收件箱和文档 patch。
9. 能通过 MCP 提供项目知识查询能力。
10. 默认不会修改源码文件，不会静默上传项目内容。

## 4. 总体阶段

建议分为 9 个阶段：

```text
阶段 0：工程骨架
阶段 1：项目扫描
阶段 2：变更雷达
阶段 3：本地 Agent 监听
阶段 4：知识收件箱
阶段 5：文档草稿与 Patch
阶段 6：本地索引与检索
阶段 7：桌面知识面板
阶段 8：MCP 与外部集成
阶段 9：产品打磨与发布
```

每个阶段都必须有可运行命令、测试样例和可验收产物。

调整原因：

变更雷达已经能识别 Git diff，但用户不应该长期手动执行 `cyclaw diff`。在进入知识收件箱前，需要先补出最小本地 Agent 监听模式，让 cyClaw 具备“自动发现变更”的产品语义。

## 5. 阶段 0：工程骨架

### 5.1 目标

建立 Rust-first 的 monorepo，确保 CLI、Core、Desktop 后续能复用同一套核心能力。

### 5.2 任务

1. 初始化 Git 仓库结构。
2. 创建 Rust workspace。
3. 创建 CLI crate。
4. 创建 core crate。
5. 创建基础错误处理、日志、配置模块。
6. 创建测试 fixtures 目录。
7. 创建基础 CI 命令。
8. 创建开发文档。

### 5.3 建议目录

```text
cyClaw/
  apps/
    desktop/
  crates/
    cyclaw-cli/
    cyclaw-core/
    cyclaw-scanner/
    cyclaw-change-radar/
    cyclaw-knowledge/
    cyclaw-docs/
    cyclaw-retrieval/
    cyclaw-policy/
    cyclaw-model/
    cyclaw-events/
    cyclaw-mcp/
  packages/
    ui/
    prompts/
    schemas/
  docs/
  fixtures/
```

### 5.4 技术选择

| 能力 | 推荐 |
| --- | --- |
| CLI | `clap` |
| 错误处理 | `anyhow` + `thiserror` |
| 日志 | `tracing` |
| 异步 | `tokio` |
| 序列化 | `serde` |
| 配置 | `figment` 或 `config` |
| 测试断言 | Rust 标准测试 + fixture |

### 5.5 验收标准

- `cargo test` 可运行。
- `cyclaw --help` 可运行。
- `cyclaw version` 输出版本信息。
- 仓库结构符合约定。

## 6. 阶段 1：项目扫描

### 6.1 目标

实现 `cyclaw init` 和 `cyclaw scan`，让 cyClaw 理解当前项目的基本结构。

### 6.2 任务

1. 检测当前目录是否为 Git 项目。
2. 识别项目语言和框架。
3. 识别依赖文件：
   - `package.json`
   - `pnpm-lock.yaml`
   - `pom.xml`
   - `build.gradle`
   - `requirements.txt`
   - `pyproject.toml`
   - `go.mod`
   - `Cargo.toml`
4. 识别文档目录：
   - `docs/`
   - `README.md`
   - `wiki/`
5. 识别配置文件：
   - `.env.example`
   - `config.*`
   - `application.yml`
   - `application.properties`
6. 生成 `.cyclaw/config.yaml`。
7. 生成 `.cyclaw/project-profile.json`。
8. 生成 `.cyclaw/project.md`。

### 6.3 输出示例

```text
.cyclaw/
  config.yaml
  project-profile.json
  project.md
```

### 6.4 验收标准

- 在至少 3 类 fixture 项目中正确识别技术栈。
- 重复运行 `cyclaw scan` 不破坏已有配置。
- 扫描结果包含语言、框架、依赖文件、文档目录、配置文件。

## 7. 阶段 2：变更雷达

### 7.1 目标

实现 `cyclaw diff`，从 Git diff 中识别哪些变化可能影响项目知识资产。

### 7.2 任务

1. 调用 `git status --porcelain`。
2. 调用 `git diff --name-status`。
3. 调用 `git diff --unified=0`。
4. 建立文件类型规则。
5. 识别变更类别：
   - API 变化。
   - 数据模型变化。
   - 依赖变化。
   - 配置变化。
   - 环境变量变化。
   - 文档变化。
6. 生成变更分析 JSON。
7. 推荐受影响知识资产。

### 7.3 初始规则

| 文件或路径 | 变更类型 | 建议知识资产 |
| --- | --- | --- |
| `*Controller*`、`routes/`、`api/` | API 变化 | `docs/api.md` |
| `models/`、`entities/`、`migrations/`、`*.sql` | 数据模型变化 | `docs/schema.md` |
| `package.json`、`pom.xml`、`go.mod`、`Cargo.toml` | 依赖变化 | `docs/dependencies.md` |
| `.env.example`、`config.*`、`application.yml` | 配置变化 | `docs/environment.md` |
| 新增顶层模块目录 | 架构变化 | `docs/architecture.md` |

### 7.4 输出示例

```text
.cyclaw/runs/{run-id}/change-analysis.json
```

### 7.5 验收标准

- 能识别 5 类基础变更。
- 能给出推荐文档路径。
- 对无关变更保持低噪音。
- 只读 Git 信息，不修改文件。

## 8. 阶段 3：本地 Agent 监听

### 8.1 目标

实现 cyClaw 的最小本地智能体运行模式，让它能够自动发现项目变更，而不是依赖用户手动执行 `cyclaw diff`。

第一版命令：

```text
cyclaw watch
```

最终产品形态：

```text
桌面端 / IDE 插件启动 Agent Runtime
Agent Runtime 自动监听项目变化
变更后自动生成 change-analysis.json
后续自动进入知识收件箱
```

### 8.2 任务

1. 增加 `cyclaw watch`。
2. 监听 Git 状态变化。
3. 发现变化后自动调用变更雷达。
4. 自动写入 `.cyclaw/runs/{run-id}/change-analysis.json`。
5. 输出本次发现的受影响知识资产。
6. 为后续桌面端提供可复用 Agent Runtime 接口。

### 8.3 第一版实现策略

第一版使用 Git 状态轮询：

```text
定期读取 git status / git diff -> 计算 fingerprint -> 变化后触发 diff 分析
```

后续再升级为：

- 文件系统事件监听。
- Git Hook。
- IDE 文件保存事件。
- 后台托盘进程。

### 8.4 验收标准

- `cyclaw watch` 能启动。
- 项目发生 Git 变更后能自动生成变更分析。
- 没有变更时不重复生成分析。
- 能通过 `--once` 做测试和调试。

当前状态：已完成最小版本。

实现补充：

- `cyclaw watch` 发现变更后会自动生成变更分析。
- 变更分析生成后会自动写入知识收件箱。
- 当前知识收件箱数据源是 `.cyclaw/knowledge-inbox.jsonl`。

## 9. 阶段 4：知识收件箱

状态：已完成基础版本。

### 9.1 目标

实现 `cyclaw inbox`，把值得沉淀的信息放入知识收件箱。

### 9.2 任务

1. 定义知识候选结构。
2. 从 `change-analysis.json` 生成候选知识。
3. 支持从任务总结文本生成候选知识。
4. 实现重要性评分。
5. 推荐写入位置。
6. 写入 `.cyclaw/knowledge-inbox.jsonl`。
7. 支持查看、接受、忽略。

### 9.3 数据结构

```json
{
  "id": "kc_001",
  "summary": "支付回调必须先校验签名再更新订单状态",
  "source_type": "task_summary",
  "source_ref": ".cyclaw/runs/2026-07-04/change-analysis.json",
  "importance": "high",
  "reasons": ["涉及支付流程", "影响数据一致性"],
  "recommended_doc": "docs/domain-rules.md",
  "related_files": ["src/payment/callback.rs"],
  "status": "pending",
  "created_at": "2026-07-04T00:00:00Z"
}
```

### 9.4 验收标准

- 能生成 pending 状态候选知识。
- 每条知识有来源、重要性、推荐写入位置。
- 支持 `cyclaw inbox list`。
- 支持 `cyclaw inbox accept <id>`。
- 支持 `cyclaw inbox ignore <id>`。

当前实现说明：

- 已新增 `cyclaw-knowledge` crate。
- 已支持从 `change-analysis.json` 生成候选知识。
- 已支持生成 `.cyclaw/knowledge-inbox.jsonl`。
- 已支持 `cyclaw inbox generate`。
- 已支持 `cyclaw inbox list`。
- 已支持 `cyclaw inbox accept <id>`。
- 已支持 `cyclaw inbox ignore <id>`。
- `cyclaw watch` 发现变更后会自动调用知识收件箱生成逻辑。

## 10. 阶段 5：文档草稿与 Patch

状态：已完成基础版本。

### 10.1 目标

实现 `cyclaw draft` 和 `cyclaw apply`，把候选知识转化为可审查的文档更新。

### 10.2 任务

1. 读取候选知识。
2. 读取目标文档。
3. 如果目标文档不存在，生成基础模板。
4. 调用模型或规则生成更新草稿。
5. 生成统一 diff。
6. 存储 patch 记录。
7. 用户确认后应用 patch。
8. 写入来源元数据。

### 10.3 输出

```text
.cyclaw/doc-patches/{patch-id}.json
docs/*.md
```

### 10.4 规则

- 不允许模型直接覆盖整篇文档。
- 所有写入必须先生成 diff。
- 默认只允许写入 `docs/` 和 `.cyclaw/`。
- 修改源码文件必须禁止。

### 10.5 验收标准

- 能为一个 pending 知识生成文档 diff。
- 能预览 diff。
- 能确认后写入。
- 能记录 patch 来源。
- 重复应用同一 patch 不产生重复内容。

当前实现说明：

- 已新增 `cyclaw-docs` crate。
- 已支持 `cyclaw draft generate`。
- 已支持 `cyclaw draft list`。
- 已支持 `cyclaw draft apply <id>`。
- 已支持从 accepted 候选知识生成文档草稿。
- 调试阶段可通过 `--include-pending` 直接处理 pending 候选知识。
- 文档草稿保存到 `.cyclaw/doc-patches/{patch-id}.json`。
- 应用草稿时只允许写入 `docs/` 和 `.cyclaw/` 下文件。
- 当前采用追加型文档更新策略，不允许模型重写整篇文档。

## 11. 阶段 6：本地索引与检索

状态：已完成基础版本。

### 11.1 目标

实现 `cyclaw index` 和 `cyclaw search`，让沉淀下来的知识可以被检索。

### 11.2 任务

1. 建立 SQLite 数据库。
2. 启用 FTS5。
3. 索引 Markdown 文档。
4. 索引知识收件箱。
5. 索引历史运行记录。
6. 支持关键词搜索。
7. 返回来源路径和片段。

### 11.3 数据库

```text
.cyclaw/index.sqlite
```

核心表：

```text
documents
document_chunks
knowledge_candidates
runs
patches
search_index
```

### 11.4 验收标准

- 能搜索 `docs/` 中的知识。
- 能搜索 `.cyclaw/memory.md`。
- 能搜索已接受的知识候选。
- 搜索结果包含来源文件和片段。
- 不依赖网络。

当前实现说明：

- 已新增 `cyclaw-retrieval` crate。
- 已使用 SQLite FTS5 建立本地全文索引。
- SQLite 使用 bundled 构建，不依赖系统 SQLite 动态库。
- 已支持 `cyclaw index`。
- 已支持 `cyclaw search <query>`。
- 已索引 `docs/`、`wiki/`、`README.md`、`.cyclaw/project.md`、`.cyclaw/knowledge-inbox.jsonl`、`.cyclaw/doc-patches/`、`.cyclaw/runs/`。
- 已生成 `.cyclaw/index.sqlite`。

## 12. 阶段 7：桌面知识面板

### 12.1 目标

实现 Tauri + React 桌面端，把 Agent Runtime 能力变成可视化工作台。

### 12.2 页面

1. 项目首页
   - 当前工作区。
   - 项目画像。
   - 文档健康概览。

2. 变更雷达
   - Git diff 摘要。
   - 变更类型。
   - 受影响知识资产。

3. 知识收件箱
   - pending 知识。
   - 重要性评分。
   - 推荐文档。
   - 接受、编辑、忽略。

4. 文档 Patch
   - 草稿内容。
   - diff 预览。
   - 应用或放弃。

5. 知识检索
   - 搜索框。
   - 来源结果。
   - 片段预览。

### 12.3 设计原则

- 不做营销首页。
- 第一屏就是项目知识状态。
- 不用聊天框作为唯一入口。
- 所有写入动作必须有 diff。
- 所有 AI 结论必须有来源。

### 12.4 验收标准

- 能选择或打开一个项目目录。
- 能展示扫描结果。
- 能展示变更分析。
- 能处理知识收件箱。
- 能预览和应用文档 patch。
- 能搜索已有知识。

## 13. 阶段 8：MCP 与外部集成

### 13.1 目标

让 Codex、Claude Code、Cursor 等工具可以调用 cyClaw 查询项目记忆。

### 13.2 任务

1. 实现 `cyclaw mcp`。
2. 提供 MCP tools：
   - `get_project_profile`
   - `search_project_knowledge`
   - `list_pending_knowledge`
   - `suggest_doc_updates`
   - `get_project_rules`
3. 提供 MCP resources：
   - `.cyclaw/project.md`
   - `.cyclaw/memory.md`
   - `docs/*`
4. 编写 Claude Code / Codex 接入说明。

### 13.3 验收标准

- MCP Server 可启动。
- 外部 AI 工具能查询项目知识。
- 查询结果包含来源。
- 不允许外部工具直接绕过确认写入文档。

## 14. 阶段 9：产品打磨与发布

### 14.1 目标

让 cyClaw 可以被真实用户安装、试用、反馈。

### 14.2 任务

1. 打包 CLI。
2. 打包桌面端。
3. 编写安装说明。
4. 编写快速开始。
5. 准备示例项目。
6. 增加错误诊断。
7. 增加日志导出。
8. 做跨平台测试：
   - Windows
   - macOS
   - Linux

### 14.3 验收标准

- 用户能在 5 分钟内完成安装和 `cyclaw init`。
- 用户能在真实项目中生成第一条知识候选。
- 用户能接受候选知识并写入文档。
- 用户能搜索刚刚写入的知识。

## 15. 测试计划

### 15.1 单元测试

重点覆盖：

- 配置读取。
- 项目扫描规则。
- Git diff 解析。
- 变更分类。
- 知识评分。
- patch 生成。
- SQLite 索引。

### 15.2 Fixture 测试

准备 fixture 项目：

```text
fixtures/
  node-api/
  java-spring/
  rust-cli/
  python-fastapi/
  go-service/
```

每个 fixture 都要包含：

- 初始代码。
- 修改后的 diff。
- 预期变更分析。
- 预期知识候选。
- 预期文档更新。

### 15.3 端到端测试

端到端链路：

```text
init -> scan -> diff -> inbox -> draft -> apply -> index -> search
```

### 15.4 桌面测试

重点检查：

- 页面不重叠。
- diff 可读。
- 长文本不溢出。
- 知识收件箱状态正确。
- 写入前确认明确。

## 16. 风险与控制

### 16.1 噪音过多

风险：

cyClaw 产生太多“建议更新文档”，用户会忽略。

控制：

- 初始规则保守。
- 引入重要性评分。
- 支持忽略规则。
- 支持项目级阈值配置。

### 16.2 文档被污染

风险：

AI 自动写入低质量内容。

控制：

- 不静默写入。
- 先进入知识收件箱。
- 必须 diff 预览。
- 用户确认后应用。

### 16.3 模型输出不稳定

风险：

文档草稿质量不一致。

控制：

- 结构化 Schema 校验。
- 模板化文档格式。
- 模型只做语义和草稿，不做边界判断。
- 规则和 AST 优先。

### 16.4 隐私问题

风险：

项目代码被上传给模型。

控制：

- 默认离线。
- 模型调用需显式配置。
- 显示将发送的上下文摘要。
- 支持本地模型。

### 16.5 Rust 开发速度

风险：

Rust-first 会降低早期迭代速度。

控制：

- 核心模块拆小。
- CLI 优先，不先做复杂 UI。
- 模型提示词和规则外置为 Markdown/YAML。
- UI 后置。

## 17. 开发优先级

P0：

- Rust workspace。
- CLI。
- 项目扫描。
- Git diff 分析。
- 本地 watch 监听。
- 知识收件箱。
- 文档 patch。

P1：

- SQLite 检索。
- Tauri 桌面面板。
- 模型网关。
- 文档健康检查。

P2：

- MCP Server。
- Git Hook。
- IDE 插件。
- 向量检索。
- 文档漂移检测。

P3：

- 团队协作。
- 云端同步。
- 技能市场。
- 后台托盘进程。

## 18. 里程碑

### M1：CLI 可跑通

交付：

- `cyclaw init`
- `cyclaw scan`
- 基础项目画像。

验收：

- 在 3 个 fixture 项目中通过。

### M2：变更雷达可用

状态：已完成基础版本。

交付：

- `cyclaw diff`
- 变更分类。
- 文档更新建议。

验收：

- 能识别 API、Schema、依赖、配置、环境变量变化。

当前实现说明：

- 已新增 `cyclaw-change-radar` crate。
- 已支持读取 `git status --porcelain` 和 `git diff --name-status`。
- 已支持识别依赖、环境变量、配置、API、Schema、文档、架构、测试和源码变更。
- 已支持生成 `.cyclaw/runs/{run-id}/change-analysis.json`。
- 已通过临时 Git 仓库验证 `package.json` 和 `.env.example` 变更识别。

### M2.5：本地 Agent 监听可用

状态：已完成最小版本。

交付：

- `cyclaw watch`
- Git 状态 fingerprint。
- 自动触发变更雷达。

验收：

- 能在项目变更后自动生成 `change-analysis.json`。
- 能通过 `--once` 调试。

### M3：知识收件箱可用

状态：已完成基础版本。

交付：

- `cyclaw inbox`
- 候选知识生成。
- 重要性评分。

验收：

- 能从 diff 和任务总结生成候选知识。

当前实现说明：

- 当前已能从 diff 生成候选知识。
- 任务总结输入留到后续模型网关或 Agent 任务记录阶段。

### M4：文档更新闭环

状态：已完成基础版本。

交付：

- `cyclaw draft`
- `cyclaw apply`
- 文档 diff。

验收：

- 能从候选知识写入 docs，并可追溯来源。

当前实现说明：

- 已能从知识收件箱候选项生成草稿。
- 已能应用草稿并写入目标文档。
- 已能更新 patch 状态为 applied。

### M5：检索闭环

状态：已完成基础版本。

交付：

- `cyclaw index`
- `cyclaw search`

验收：

- 能搜索文档和已接受知识。

当前实现说明：

- 已能搜索 Markdown 文档、知识收件箱、文档草稿和变更分析。
- 搜索结果包含来源路径、类型、标题和片段。

### M6：桌面 MVP

交付：

- Tauri 桌面端。
- 变更雷达页面。
- 知识收件箱页面。
- 文档 patch 页面。

验收：

- 非命令行用户也能完成核心闭环。

### M7：AI 工具集成

交付：

- `cyclaw mcp`
- MCP 查询项目知识。

验收：

- Claude Code / Codex / Cursor 能通过 MCP 查询项目规则和历史知识。

## 19. 当前最应该先做什么

当前 M1、M2、M2.5、M3、M4 和 M5 基础版本已完成。下一步建议进入桌面知识面板：

```text
创建 Tauri + React 桌面端
启动或连接 cyClaw Agent Runtime
展示变更雷达、知识收件箱、文档草稿和搜索结果
```

仍然不要先做模型网关。

原因：

- 当前核心数据闭环已经跑通。
- 下一步需要把命令行能力变成用户可用的本地 Agent 面板。
- 桌面端是后续 IDE 插件和 MCP 集成前最直观的产品载体。
