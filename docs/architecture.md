# cyClaw 架构概览

## 设计目标

cyClaw 是 Coding Agent 的项目记忆、事实治理与上下文控制层。核心运行在本地，负责在不注入整个仓库的前提下，为 Agent 提供可追溯的项目事实和受控的文档治理能力。

## 分层

```text
CLI / MCP / VS Code / IDEA / Skills
              |
        cyclaw-core
  ┌───────────┼────────────┐
扫描与变更   事实与任务    文档与检索
radar        memory       docs/retrieval
  └───────────┼────────────┘
       policy / events / model
              |
       <project>/.cyclaw/
```

## 核心模块

| 模块 | 职责 |
| --- | --- |
| `cyclaw-cli` | 命令行入口、任务命令、Fact 五种治理快捷命令、证据验证历史、Fact Patch 过滤分页、Watch 和本地 MCP 服务启动。 |
| `cyclaw-core` | 项目生命周期、上下文编译、Fact Patch 与文档 Patch 治理编排。 |
| `cyclaw-memory` | Task Record、Fact Ledger、跨文件事务恢复、结构化证据验证账本、跨任务召回和知识对账。 |
| `cyclaw-mcp` | 面向 Coding Agent 的本地 stdio JSON-RPC 接口，所有事实写入复用 Core，并为事实工具声明严格 Schema。 |
| `cyclaw-policy` | 文档写入、模型联网、自动化、路径和 Shell 权限。 |
| `cyclaw-events` | 追加式审计事件。 |
| `cyclaw-change-radar` | 文件事件、Git 快照和变更分类。 |
| `cyclaw-docs` | Markdown 章节解析和五种文档治理操作。 |
| `cyclaw-retrieval` | SQLite FTS5 索引、字面量安全查询和项目知识检索。 |
| `cyclaw-model` | OpenAI-compatible Provider 配置、限流、重试和缓存。 |

Fact Patch 在应用和撤销前先写入 `.cyclaw/memory/fact-transactions/` 事务日志，并进入 `applying` 或 `reverting` 中间状态。MCP 启动、草稿列表及后续治理操作会恢复中断事务：账本匹配操作前快照时幂等重放，匹配操作后快照时补齐 Patch 状态；两者都不匹配则拒绝恢复，避免覆盖并发修改。

证据验证结果追加到 `.cyclaw/memory/evidence-verifications.jsonl`，不会修改 Fact 本体。`FactEvidence.hash_scope` 支持 `file`、`line_range` 和 `symbol`；文件级 SHA-256 采用流式读取，单个证据文件默认限制为 16 MiB。FTS5 查询会先转换为安全字面量词项，解析类错误降级到转义 LIKE，其他数据库错误继续向调用方返回。

## 数据边界

所有运行状态按项目写入 `<project>/.cyclaw/`。任务记录、Fact Ledger、对账报告、候选、Patch、索引和事件均不跨项目共享。模型调用只发生在用户配置 Provider 并授权后。

详细设计、技术选择和阶段计划见 [技术架构](cyclaw-tech-architecture-v0.1.md)、[运行时设计](cyclaw-agent-runtime-design-v0.1.md) 和 [Agent 集成方案](cyclaw-agent-integrations-v0.1.md)。
