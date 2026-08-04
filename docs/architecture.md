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
| `cyclaw-cli` | 命令行入口、任务命令、Fact 五种治理快捷命令、结构化证据参数、证据验证分页与账本诊断、事务诊断、Fact Patch 过滤分页、Watch 和本地 MCP 服务启动。 |
| `cyclaw-core` | 项目生命周期、上下文编译、Fact Patch 与文档 Patch 治理编排，并统一暴露事务恢复诊断、最近恢复报告和证据验证分页。 |
| `cyclaw-memory` | Task Record、Fact Ledger、跨文件事务恢复与诊断、最近恢复报告、结构化证据验证账本、归档 sidecar 索引及损坏恢复、跨任务召回和知识对账。 |
| `cyclaw-mcp` | 面向 Coding Agent 的本地 stdio JSON-RPC 接口，所有事实写入复用 Core；`doctor` 区分必需健康检查与可选能力降级，并暴露事务和最近恢复诊断。 |
| `cyclaw-policy` | 文档写入、模型联网、自动化、路径和 Shell 权限，以及跨进程项目锁。 |
| `cyclaw-events` | 追加式审计事件，包括 Fact Patch 创建、应用、撤销、恢复完成和恢复阻塞。 |
| `cyclaw-change-radar` | 文件事件、Git 快照和变更分类。 |
| `cyclaw-docs` | Markdown 章节解析和五种文档治理操作。 |
| `cyclaw-retrieval` | SQLite FTS5 索引、字面量安全查询和项目知识检索。 |
| `cyclaw-model` | OpenAI-compatible Provider 配置、限流、重试和缓存。 |

Fact 与 Fact Patch ID 在存储层执行长度、字符集和前缀校验，Patch 与事务文件还会校验文件名和内部 ID 一致，防止外部接口输入形成路径穿越。Fact Patch 在应用和撤销前先写入 `.cyclaw/memory/fact-transactions/` 事务日志，并进入 `applying` 或 `reverting` 中间状态。恢复过程逐条隔离事务：账本匹配操作前快照时幂等重放，匹配操作后快照时补齐 Patch 状态；两者都不匹配或日志损坏时保留原日志并报告阻塞，不影响 MCP 启动和只读诊断。恢复结果原子写入 `.cyclaw/memory/latest-fact-recovery.json`，完成与阻塞分别产生审计事件；相同阻塞结果不会重复发事件。写操作会在存在阻塞事务时拒绝继续，避免覆盖并发修改。Windows 下已存在锁文件可能表现为 `PermissionDenied`，项目锁仅在路径确实存在时将其视为正常竞争。

证据验证结果追加到 `.cyclaw/memory/evidence-verifications.jsonl`，不会修改 Fact 本体。主账本达到 4 MiB 时滚动到 `.cyclaw/memory/evidence-verifications/`，默认保留最近 12 个归档。新归档同时生成 `.index.json` sidecar，记录 Fact ID、验证时间和 JSONL 字节范围；查询先用索引完成过滤、排序和总数计算，只读取当前页对应的记录。旧归档、损坏索引或元数据不匹配的索引自动回退严格全量解析，归档裁剪时同步删除 sidecar。主账本因进程中断留下的无换行不完整尾行会在读取时跳过，并在下次持锁追加前截断；归档损坏或完整坏行严格报错。`FactEvidence.hash_scope` 支持 `file`、`line_range` 和 `symbol`；CLI 可直接提供符号、行范围、哈希范围和证据类型。文件级 SHA-256 采用流式读取，单个证据文件默认限制为 16 MiB。

知识对账区分有效期、证据消失和证据漂移：`valid_until` 到期具有最高优先级并产生 `stale/delete` 建议；未到期事实只有在全部可验证证据缺失时才建议删除；`hash_mismatch` 和 `invalid_location` 产生 `evidence_drift/update` 建议；`outside_project`、`unsupported` 和 `too_large` 不触发破坏性建议。VS Code 同步展示漂移计数。FTS5 查询会先转换为安全字面量词项，解析类错误降级到转义 LIKE，其他数据库错误继续向调用方返回。

## 数据边界

所有运行状态按项目写入 `<project>/.cyclaw/`。任务记录、Fact Ledger、对账报告、候选、Patch、索引和事件均不跨项目共享。模型调用只发生在用户配置 Provider 并授权后。

详细设计、技术选择和阶段计划见 [技术架构](cyclaw-tech-architecture-v0.1.md)、[运行时设计](cyclaw-agent-runtime-design-v0.1.md) 和 [Agent 集成方案](cyclaw-agent-integrations-v0.1.md)。
