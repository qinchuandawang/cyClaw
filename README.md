# cyClaw

cyClaw 是 AI Coding 时代的项目知识维护层。

当前阶段目标：

- 建立 Rust-first 工程骨架。
- 提供 `cyclaw init` 初始化命令。
- 提供 `cyclaw scan` 项目扫描命令。
- 提供 `cyclaw diff` 变更雷达命令。
- 生成 `.cyclaw/project-profile.json` 和 `.cyclaw/project.md`。
- 生成 `.cyclaw/runs/{run-id}/change-analysis.json`。

## 开发命令

```bash
cargo test
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
cargo run -p cyclaw-cli -- diff
```
