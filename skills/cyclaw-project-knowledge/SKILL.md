---
name: cyclaw-project-knowledge
description: Maintain a project's non-code knowledge assets through the cyClaw MCP. Use after code, API, schema, dependency, configuration, architecture, or deployment changes; when the user asks to update project documentation; or when a coding task should preserve decisions and operational knowledge.
---

# cyClaw Project Knowledge

Use cyClaw MCP as the only knowledge mutation layer. Do not write generated documentation directly unless the user explicitly bypasses cyClaw.

## Workflow

1. Call `doctor`. Stop on Git, initialization, or policy failures.
2. Call `get_active_task`. If no task exists, call `begin_task` with the user's objective and relevant files.
3. Call `get_task_context` before changing code. Treat active constraints, decisions, and failed approaches as higher priority than generic documentation.
4. Call `get_project_status` and `get_policy`.
5. Call `analyze_changes` once.
4. Call `list_pending_knowledge`.
6. For each relevant new candidate, call `get_candidate_detail`, then call `search_project_knowledge` for the target subject before creating a patch.
7. Record important architectural or behavioral decisions with `record_decision`. Record rejected implementations that may be attempted again with `record_failed_approach`.
8. Choose exactly one knowledge operation:
   - `create`: no canonical section exists.
   - `update`: an existing section remains valid but its content changed.
   - `merge`: two or more sections contain overlapping knowledge.
   - `supersede`: preserve an old decision or constraint as history while replacing it with a new one.
   - `delete`: the section or document is obsolete and retaining it would mislead future agents.
9. Follow the current runtime strategy:
   - Observe: report candidates only.
   - Review: call `review_candidate` with `accept` and `generate_draft=true`, then call `preview_document_patch`.
   - Smart: prefer model-reviewed `keep` candidates; preview before applying.
   - Auto: allow cyClaw policy and confidence threshold to decide; do not bypass failed checks.
10. Call `preview_document_patch` with the selected operation, section selectors, merge sources, and replacement content. Prefer `update`, `merge`, `supersede`, or `delete` over `create` when existing knowledge already covers the subject.
11. Call `apply_document_patch` only when policy permits and the strategy or user authorizes writing.
12. For long tasks, call `checkpoint_task`. Before completion call `reconcile_project_knowledge`, then `close_task` with a compact handoff summary.
13. Report the operation, changed knowledge assets, skipped candidates, patch IDs, and revert availability.

## Rules

- Never call arbitrary shell or filesystem write tools to imitate cyClaw document operations.
- Preserve low-confidence candidates for review unless the user requests ignoring them.
- Do not append a new section merely because writing is easier than reconciling existing knowledge.
- Use `revert_document_patch` when the applied result is wrong.
- Cite candidate IDs, target documents, and evidence files in the final summary.
