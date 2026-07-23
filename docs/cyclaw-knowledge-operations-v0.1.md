# cyClaw 知识治理操作 v0.1

日期：2026-07-19

## 1. 目标

知识维护不等于持续追加 Markdown。每次生成草稿前，cyClaw 应先判断现有知识是否需要新增、更新、合并、取代或删除。

所有操作统一经过：

```text
候选知识 -> 选择操作 -> Markdown 章节解析 -> Patch 预览
-> 权限检查 -> 并发修改检查 -> 应用 -> 事件审计 -> 可撤销
```

## 2. 操作定义

### create

仅在现有文档没有相关知识时新增章节。默认生成标准候选章节。

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation create
```

### update

替换一个现有 Markdown 章节。`selector` 使用章节标题，不包含 `#`。

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation update `
  --selector "依赖管理" `
  --replacement-file .\updated-section.md
```

也可以使用候选标记定位 cyClaw 生成的章节：

```text
candidate:<候选ID>
```

### merge

将两个或更多重复章节合并为一个章节。

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation merge `
  --selector "依赖说明" `
  --source-selector "依赖版本" `
  --source-selector "构建依赖" `
  --replacement-file .\merged-section.md
```

选择器必须分别命中唯一章节，互相嵌套的父子章节不能直接合并。

### supersede

保留旧章节及历史内容，增加“已被取代”标记，并插入新章节。适合 ADR、业务规则和兼容策略。

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation supersede `
  --selector "旧鉴权方案" `
  --replacement-file .\new-auth-decision.md
```

### delete

删除一个失效章节：

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation delete `
  --selector "废弃配置"
```

删除整份目标文档：

```powershell
cyclaw draft generate `
  --candidate <候选ID> `
  --operation delete `
  --delete-document
```

整文档删除仍会保存原始内容，应用后可以撤销恢复。

## 3. Codex MCP

Codex 通过 `preview_document_patch` 使用五种操作。例如更新章节：

```json
{
  "candidate_id": "kc_example",
  "operation": "update",
  "selector": "依赖管理",
  "replacement_content": "## 依赖管理\n\n当前项目统一使用 pnpm 11。"
}
```

合并章节：

```json
{
  "candidate_id": "kc_example",
  "operation": "merge",
  "selector": "依赖说明",
  "source_selectors": ["依赖版本", "构建依赖"],
  "replacement_content": "## 依赖与构建\n\n统一后的事实。"
}
```

删除整份文档：

```json
{
  "candidate_id": "kc_example",
  "operation": "delete",
  "delete_target_document": true
}
```

`review_candidate` 在 `generate_draft=true` 时接受同样的操作参数。

## 4. 自动操作选择

未显式指定操作时：

- 目标文档中存在与候选摘要同标题的章节：生成 `update`。
- 没有同标题章节：生成 `create`。

`merge`、`supersede` 和 `delete` 不自动猜测，必须由 Codex或开发者基于证据明确选择。

## 5. 安全约束

- 显式操作必须指定单个候选 ID。
- update、supersede 和章节 delete 必须唯一命中章节。
- merge 至少需要两个不同选择器。
- delete-document 只能与 delete 同时使用。
- 应用前检查文档是否在预览后发生变化。
- 撤销前检查文档是否在应用后再次变化。
- 所有操作必须通过 DocsWrite 和 `allow_docs_apply` 权限。
- 所有应用和撤销写入事件日志。

## 6. Fact Ledger 对账

文档治理操作与结构化事实对账共享同一组语义：

| 对账发现 | 默认建议 | 含义 |
| --- | --- | --- |
| duplicate | merge | 多条事实表达同一约束，应合并来源和证据 |
| conflict | supersede | 新事实与旧事实冲突，应明确当前生效项及取代关系 |
| stale | delete | 来源文件已不存在或事实已失效，应从有效上下文中移除 |

执行：

```powershell
cyclaw task reconcile
cyclaw task latest-reconciliation
```

当前对账只生成报告和治理建议，不自动改写 `.cyclaw/memory/facts.jsonl`，也不直接应用文档 Patch。中文重复检测采用中文双字切分与相似度阈值；冲突检测属于启发式结果，必须结合来源文件和当前代码人工或由 Codex 审阅。
