# cyClaw

cyClaw 是 AI Coding 时代的项目知识维护层。

当前阶段目标：

- 建立 Rust-first 工程骨架。
- 提供 `cyclaw init` 初始化命令。
- 提供 `cyclaw scan` 项目扫描命令。
- 提供 `cyclaw status` 项目状态命令。
- 提供 `cyclaw diff` 变更雷达命令。
- 提供 `cyclaw watch` 本地监听命令。
- 提供 `cyclaw inbox` 知识收件箱命令。
- 提供 `cyclaw draft` 文档草稿命令。
- 提供 `cyclaw index` 和 `cyclaw search` 本地检索命令。
- 生成 `.cyclaw/project-profile.json` 和 `.cyclaw/project.md`。
- 生成 `.cyclaw/runs/{run-id}/change-analysis.json`。
- 生成 `.cyclaw/knowledge-inbox.jsonl`。
- 生成 `.cyclaw/doc-patches/*.json`。
- 生成 `.cyclaw/index.sqlite`。

说明：

`cyclaw` CLI 是底层工程入口和调试入口，不是最终用户的主要使用方式。cyClaw 的目标形态是本地常驻的项目知识智能体：打开项目后自动监听代码和配置变化，主动生成知识资产更新建议，再通过桌面端、IDE 或 MCP 暴露结果。

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
```

## CLI 完整验证

当前 CLI/Core 的完整本地流程：

```bash
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- watch --once --interval 0
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

这个脚本会创建临时 Git 项目，制造依赖和环境变量变更，并跑通 init、scan、watch、inbox、draft、index、search、status 全链路。
