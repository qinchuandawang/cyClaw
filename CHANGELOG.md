# 更新日志

本项目遵循语义化版本。`0.x` 阶段仍可能调整 MCP 工具、CLI 参数和 `.cyclaw/` 存储格式；破坏性变更会在 Release Notes 中明确说明迁移方式。

## [0.1.6] - 2026-07-22

### 新增

- 任务生命周期协议：开始任务、上下文编译、决策/失败方案记录、检查点、知识对账和关闭任务。
- 跨任务 Fact Ledger，以及重复、冲突、失效事实的治理建议。
- VS Code 任务中心和 Codex MCP 任务工具。
- GitHub 开源基线：CI、Release、社区、安全和依赖更新配置。

### 变更

- 文档 Patch 支持 `create`、`update`、`merge`、`supersede`、`delete`。
- Watch 改为文件系统事件驱动，并保留 Git 快照去重。

### 已知限制

- 官方 VSIX 当前只内置 Windows x64 CLI。
- 知识冲突识别是启发式建议，必须结合证据人工或由 Agent 审阅。
