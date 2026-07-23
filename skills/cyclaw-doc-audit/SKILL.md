---
name: cyclaw-doc-audit
description: Audit documentation drift and project knowledge gaps through the cyClaw MCP. Use before releases, major merges, project handoffs, architecture reviews, periodic maintenance, or when code behavior and existing docs may no longer agree.
---

# cyClaw Documentation Audit

Audit before mutating. Prefer evidence-backed, reviewable patches.

## Workflow

1. Call `doctor`, `get_project_profile`, `get_project_status`, `get_policy`, and `list_project_facts`.
2. Call `search_project_knowledge` for API, schema, dependencies, environment, architecture, deployment, and known limitations.
3. Call `analyze_changes`, `list_pending_knowledge`, and `list_document_patches`.
4. Call `reconcile_project_knowledge`, then classify findings as missing, stale, duplicate, conflict, unsupported, or current.
5. Inspect each actionable candidate with `get_candidate_detail`.
6. Assign a remediation operation to every actionable finding: `update` stale content, `merge` duplicates, `supersede` replaced decisions, `delete` misleading obsolete content, and `create` only for genuinely missing knowledge.
7. Generate drafts with `preview_document_patch`; do not apply during an audit unless the user explicitly requests remediation and permissions allow it.
8. Produce an audit summary ordered by risk:
   - incorrect or dangerous documentation;
   - missing operational knowledge;
   - stale API/schema/configuration content;
   - duplicate or low-value content;
   - healthy areas.

Use candidate IDs and source paths as evidence. Do not infer correctness from file names alone when stronger project knowledge is available.
