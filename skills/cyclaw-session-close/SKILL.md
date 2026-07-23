---
name: cyclaw-session-close
description: Close a coding session with a cyClaw knowledge check. Use before ending a long implementation task, handing work to another developer or agent, preparing a commit or pull request, or when important decisions, failed approaches, limitations, environment changes, and follow-up work may otherwise be forgotten.
---

# cyClaw Session Close

Run a short knowledge-maintenance pass before declaring the coding task complete.

## Workflow

1. Call `get_active_task`, `get_project_status`, and `analyze_changes`.
2. Call `list_pending_knowledge` and inspect newly added candidates with `get_candidate_detail`.
3. Check specifically for API, schema, dependency, environment, architecture, deployment, limitation, and migration knowledge.
4. Search existing knowledge with `search_project_knowledge` before accepting a candidate that may duplicate existing documentation.
5. Classify each important candidate as `create`, `update`, `merge`, `supersede`, or `delete` before generating a draft.
6. For important candidates, call `review_candidate` with `accept` and the selected operation. Use selectors and replacement content to reconcile existing knowledge instead of appending duplicates.
7. Preview each draft. Apply only when the current strategy and permissions permit.
8. Call `reconcile_project_knowledge` and review duplicate, conflict, and stale findings.
9. Call `close_task` with a compact handoff containing:
   - knowledge assets updated;
   - drafts awaiting review;
   - ignored or duplicate candidates;
   - unresolved risks and next actions.

Do not mark the development task fully closed when high-confidence candidates remain unreviewed without explanation.
