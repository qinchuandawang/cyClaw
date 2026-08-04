use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cyclaw_agent::{AgentRunOptions, run_agent_once};
use cyclaw_core::{
    BeginTaskOptions, DraftOptions, FactEvidence, FactInput, FactOperation, FactPatchQuery,
    FactPatchRequest, FactPatchStatus, FactType, ProjectFact, SearchOptions,
    apply_document_patch as apply_patch, apply_fact_patch as apply_fact_patch_core,
    begin_task as begin_project_task, checkpoint_task as checkpoint_project_task,
    close_task as close_project_task,
    diagnose_fact_patch_transactions as core_diagnose_fact_transactions, generate_document_drafts,
    get_active_task as core_get_active_task,
    get_latest_fact_recovery_report as core_get_latest_fact_recovery,
    get_latest_reconciliation as core_get_latest_reconciliation,
    get_task_context as core_get_task_context, list_document_patches, list_inbox,
    list_project_facts as core_list_project_facts, list_tasks as core_list_tasks,
    preview_fact_patch as preview_fact_patch_core, project_fact_from_input, project_status,
    query_evidence_verifications as core_query_evidence_verifications,
    query_fact_patches as core_query_fact_patches,
    reconcile_project_knowledge as core_reconcile_project_knowledge, record_task_decision,
    record_task_failed_approach, recover_fact_patch_transactions as recover_fact_transactions,
    revert_document_patch as revert_patch, revert_fact_patch as revert_fact_patch_core,
    search_project, update_inbox_status, verify_fact_evidence as verify_fact_evidence_core,
    watch_project_once,
};
use cyclaw_docs::{DocumentPatchStatus, KnowledgeOperation};
use cyclaw_events::read_events;
use cyclaw_knowledge::KnowledgeStatus;
use cyclaw_model::list_providers;
use cyclaw_policy::{load_or_default, set_auto_apply_min_confidence, set_permission};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct McpServerOptions {
    pub project_root: PathBuf,
}

impl McpServerOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

pub fn run_stdio_server(options: McpServerOptions) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_server(options, stdin.lock(), stdout.lock())
}

pub fn run_server<R, W>(options: McpServerOptions, reader: R, mut writer: W) -> Result<()>
where
    R: Read,
    W: Write,
{
    let mut transport = McpTransport::new(reader);
    // 坏事务由 doctor 报告，不能阻止 MCP 启动和只读诊断。
    let _ = recover_fact_transactions(&options.project_root)?;
    let server = McpServer::new(options.project_root);

    while let Some(request) = transport.next_message()? {
        let Some(response) = server.handle_message(request)? else {
            continue;
        };
        write_message(&mut writer, &response)?;
    }

    writer.flush()?;
    Ok(())
}

struct McpServer {
    project_root: PathBuf,
}

impl McpServer {
    fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    fn handle_message(&self, request: Value) -> Result<Option<Value>> {
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if id.is_none() {
            return Ok(None);
        }

        let id = id.unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize_result()),
            "tools/list" => Ok(json!({ "tools": self.tools() })),
            "tools/call" => self.call_tool(request.get("params").cloned().unwrap_or(Value::Null)),
            "resources/list" => self.list_resources(),
            "resources/read" => {
                self.read_resource(request.get("params").cloned().unwrap_or(Value::Null))
            }
            _ => Err(anyhow::anyhow!("不支持的 MCP 方法: {}", method)),
        };

        Ok(Some(match result {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32603,
                    "message": error.to_string()
                }
            }),
        }))
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {},
                "resources": {}
            },
            "serverInfo": {
                "name": "cyclaw",
                "version": env!("CARGO_PKG_VERSION")
            }
        })
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = vec![
            json!({
                "name": "get_project_status",
                "description": "读取 cyClaw 当前项目状态、待处理知识和建议下一步。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "get_project_profile",
                "description": "读取 .cyclaw/project-profile.json 中的项目画像。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "search_project_knowledge",
                "description": "搜索本地项目知识索引，返回带来源路径的结果。",
                "inputSchema": object_schema(vec![
                    ("query", json!({ "type": "string", "description": "搜索关键词" })),
                    ("limit", json!({ "type": "integer", "description": "返回数量上限", "minimum": 1, "maximum": 50 }))
                ])
            }),
            json!({
                "name": "list_pending_knowledge",
                "description": "列出知识收件箱中待处理的候选知识。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "list_document_patches",
                "description": "列出待应用的文档草稿。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "get_policy",
                "description": "读取当前项目权限策略，不包含任何 API Key。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "get_model_providers",
                "description": "读取当前项目的模型 Provider 配置，不包含 API Key。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "list_events",
                "description": "读取 cyClaw 事件日志，帮助模型理解知识资产的演进过程。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100 }))])
            }),
            json!({
                "name": "list_agent_runs",
                "description": "读取最近 Agent 运行记录。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 50 }))])
            }),
            json!({"name":"analyze_changes","description":"执行一次增量知识分析，生成新的候选知识。","inputSchema":object_schema(vec![])}),
            json!({"name":"get_candidate_detail","description":"按 ID 读取候选知识、证据和已有草稿。","inputSchema":object_schema(vec![("candidate_id",json!({"type":"string"}))])}),
            json!({"name":"preview_document_patch","description":"为候选生成或读取文档草稿，支持 create/update/merge/supersede/delete，不修改目标文档。","inputSchema":object_schema(vec![("candidate_id",json!({"type":"string"})),("operation",json!({"type":"string","enum":["create","update","merge","supersede","delete"]})),("selector",json!({"type":"string","description":"Markdown 章节标题或 candidate:<ID>"})),("source_selectors",json!({"type":"array","items":{"type":"string"}})),("replacement_content",json!({"type":"string"})),("delete_target_document",json!({"type":"boolean"}))])}),
            json!({"name":"review_candidate","description":"接受或忽略候选；接受时可用五种知识操作生成草稿。","inputSchema":object_schema(vec![("candidate_id",json!({"type":"string"})),("action",json!({"type":"string","enum":["accept","ignore"]})),("generate_draft",json!({"type":"boolean"})),("operation",json!({"type":"string","enum":["create","update","merge","supersede","delete"]})),("selector",json!({"type":"string"})),("source_selectors",json!({"type":"array","items":{"type":"string"}})),("replacement_content",json!({"type":"string"})),("delete_target_document",json!({"type":"boolean"}))])}),
            json!({"name":"apply_document_patch","description":"应用指定文档草稿，受文档写入权限约束。","inputSchema":object_schema(vec![("patch_id",json!({"type":"string"}))])}),
            json!({"name":"revert_document_patch","description":"撤销已应用的文档草稿，恢复原始内容。","inputSchema":object_schema(vec![("patch_id",json!({"type":"string"}))])}),
            json!({"name":"list_fact_patches","description":"分页列出结构化事实治理草稿，可按状态和操作过滤。","inputSchema":object_schema(vec![("status",json!({"type":"string","enum":["pending","applying","applied","reverting","reverted"]})),("operation",json!({"type":"string","enum":["create","update","merge","supersede","delete"]})),("offset",json!({"type":"integer","minimum":0})),("limit",json!({"type":"integer","minimum":1,"maximum":200}))])}),
            json!({"name":"preview_fact_patch","description":"预览结构化事实的 create/update/merge/supersede/delete 操作，生成可审计且可撤销的草稿，不修改 Fact Ledger。","inputSchema":object_schema_with_required(vec![("operation",json!({"type":"string","enum":["create","update","merge","supersede","delete"]})),("target_fact_id",json!({"type":"string"})),("source_fact_ids",json!({"type":"array","items":{"type":"string"}})),("fact",json!({"type":"object","description":"create、update、supersede 的完整事实快照；merge 时可选，用于更新主事实。"}))], &["operation"])}),
            json!({"name":"apply_fact_patch","description":"应用指定事实治理草稿，应用前校验事实预览指纹。","inputSchema":object_schema_with_required(vec![("patch_id",json!({"type":"string"}))], &["patch_id"])}),
            json!({"name":"revert_fact_patch","description":"撤销指定已应用事实草稿；仅在应用后事实未变化时执行。","inputSchema":object_schema_with_required(vec![("patch_id",json!({"type":"string"}))], &["patch_id"])}),
            json!({"name":"verify_fact_evidence","description":"验证结构化事实证据并追加独立验证账本，不修改 Fact Ledger。","inputSchema":object_schema_with_required(vec![("fact_id",json!({"type":"string"}))], &["fact_id"])}),
            json!({"name":"list_evidence_verifications","description":"分页读取独立事实证据验证记录，并返回总数和账本损坏诊断。","inputSchema":object_schema(vec![("fact_id",json!({"type":"string"})),("offset",json!({"type":"integer","minimum":0})),("limit",json!({"type":"integer","minimum":1,"maximum":200}))])}),
            json!({"name":"list_fact_transactions","description":"只读诊断待恢复的 Fact Patch 事务，不修改账本。","inputSchema":object_schema(vec![])}),
            fact_operation_tool("create_fact", "生成新增事实草稿", false, false),
            fact_operation_tool("update_fact", "生成更新事实草稿", true, false),
            fact_operation_tool("merge_facts", "生成合并事实草稿", true, true),
            fact_operation_tool("supersede_fact", "生成取代事实草稿", true, false),
            fact_operation_tool("delete_fact", "生成逻辑删除事实草稿", true, false),
            json!({"name":"run_agent","description":"运行一次 cyClaw Agent，可选择是否调用活动模型。","inputSchema":object_schema(vec![("use_model",json!({"type":"boolean"})),("provider",json!({"type":"string"}))])}),
            json!({"name":"set_runtime_strategy","description":"设置观察、审阅、智能审阅或自动文档策略。","inputSchema":object_schema(vec![("strategy",json!({"type":"string","enum":["observe","review","smart","auto"]})),("auto_apply_min_confidence",json!({"type":"integer","minimum":0,"maximum":100}))])}),
            json!({"name":"doctor","description":"诊断 Git、配置、模型、权限、知识目录和文档写入状态。","inputSchema":object_schema(vec![])}),
            json!({"name":"begin_task","description":"开始一个项目任务，并返回首个带证据的上下文包。","inputSchema":object_schema(vec![("title",json!({"type":"string"})),("objective",json!({"type":"string"})),("related_files",json!({"type":"array","items":{"type":"string"}})),("context_budget_tokens",json!({"type":"integer","minimum":256,"maximum":16000}))])}),
            json!({"name":"get_active_task","description":"读取当前活动任务；没有活动任务时返回 null。","inputSchema":object_schema(vec![])}),
            json!({"name":"get_task_context","description":"为当前或指定任务编译事实、文档和候选组成的最小上下文包。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("query",json!({"type":"string"})),("budget_tokens",json!({"type":"integer","minimum":256,"maximum":16000})),("limit",json!({"type":"integer","minimum":1,"maximum":50}))])}),
            json!({"name":"record_decision","description":"把任务中的关键决策写入任务记录和结构化 Fact Ledger。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("statement",json!({"type":"string"})),("rationale",json!({"type":"string"})),("evidence",json!({"type":"array","items":{"type":"string"}})),("confidence",json!({"type":"integer","minimum":0,"maximum":100}))])}),
            json!({"name":"record_failed_approach","description":"记录尝试过但失败的方案、原因和证据，供后续会话避免重复。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("approach",json!({"type":"string"})),("reason",json!({"type":"string"})),("evidence",json!({"type":"array","items":{"type":"string"}}))])}),
            json!({"name":"checkpoint_task","description":"记录长任务检查点、当前结论和相关文件。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("summary",json!({"type":"string"})),("related_files",json!({"type":"array","items":{"type":"string"}}))])}),
            json!({"name":"reconcile_project_knowledge","description":"检测结构化事实中的重复、冲突、证据漂移和失效，并推荐 merge/supersede/update/delete。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"}))])}),
            json!({"name":"close_task","description":"关闭当前任务，可同时执行知识对账并返回交接信息。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("summary",json!({"type":"string"})),("reconcile",json!({"type":"boolean"}))])}),
            json!({"name":"list_project_facts","description":"读取结构化项目事实，可按状态限制数量。","inputSchema":object_schema(vec![("limit",json!({"type":"integer","minimum":1,"maximum":200}))])}),
            json!({"name":"list_tasks","description":"读取最近项目任务记录。","inputSchema":object_schema(vec![("limit",json!({"type":"integer","minimum":1,"maximum":100}))])}),
            json!({"name":"get_latest_reconciliation","description":"读取最近一次知识对账报告。","inputSchema":object_schema(vec![])}),
        ];
        for tool in &mut tools {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let write = is_write_tool(name);
            let destructive = matches!(
                name,
                "apply_document_patch"
                    | "revert_document_patch"
                    | "apply_fact_patch"
                    | "revert_fact_patch"
                    | "create_fact"
                    | "update_fact"
                    | "merge_facts"
                    | "supersede_fact"
                    | "delete_fact"
                    | "set_runtime_strategy"
            );
            let idempotent = matches!(
                name,
                "get_project_status"
                    | "get_project_profile"
                    | "search_project_knowledge"
                    | "list_pending_knowledge"
                    | "list_document_patches"
                    | "get_policy"
                    | "get_model_providers"
                    | "list_events"
                    | "list_agent_runs"
                    | "get_candidate_detail"
                    | "list_fact_patches"
                    | "list_evidence_verifications"
                    | "list_fact_transactions"
                    | "doctor"
            );
            if let Some(object) = tool.as_object_mut() {
                object.insert(
                    "annotations".to_string(),
                    json!({
                        "readOnlyHint": !write,
                        "destructiveHint": destructive,
                        "idempotentHint": idempotent,
                        "openWorldHint": false
                    }),
                );
            }
        }
        tools
    }

    fn call_tool(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .context("缺少工具名称")?;
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

        let payload = match name {
            "get_project_status" => self.get_project_status()?,
            "get_project_profile" => self.get_project_profile()?,
            "search_project_knowledge" => self.search_project_knowledge(arguments)?,
            "list_pending_knowledge" => self.list_pending_knowledge()?,
            "list_document_patches" => self.list_document_patches()?,
            "get_policy" => self.get_policy()?,
            "get_model_providers" => self.get_model_providers()?,
            "list_events" => self.list_events(arguments)?,
            "list_agent_runs" => self.list_agent_runs(arguments)?,
            "analyze_changes" => self.analyze_changes()?,
            "get_candidate_detail" => self.get_candidate_detail(arguments)?,
            "preview_document_patch" => self.preview_document_patch(arguments)?,
            "review_candidate" => self.review_candidate(arguments)?,
            "apply_document_patch" => self.apply_document_patch(arguments)?,
            "revert_document_patch" => self.revert_document_patch(arguments)?,
            "list_fact_patches" => self.list_fact_patches(arguments)?,
            "preview_fact_patch" => self.preview_fact_patch(arguments)?,
            "apply_fact_patch" => self.apply_fact_patch(arguments)?,
            "revert_fact_patch" => self.revert_fact_patch(arguments)?,
            "verify_fact_evidence" => self.verify_fact_evidence(arguments)?,
            "list_evidence_verifications" => self.list_evidence_verifications(arguments)?,
            "list_fact_transactions" => self.list_fact_transactions()?,
            "create_fact" => self.fact_operation(FactOperation::Create, arguments)?,
            "update_fact" => self.fact_operation(FactOperation::Update, arguments)?,
            "merge_facts" => self.fact_operation(FactOperation::Merge, arguments)?,
            "supersede_fact" => self.fact_operation(FactOperation::Supersede, arguments)?,
            "delete_fact" => self.fact_operation(FactOperation::Delete, arguments)?,
            "run_agent" => self.run_agent(arguments)?,
            "set_runtime_strategy" => self.set_runtime_strategy(arguments)?,
            "doctor" => self.doctor()?,
            "begin_task" => self.begin_task(arguments)?,
            "get_active_task" => self.get_active_task()?,
            "get_task_context" => self.get_task_context(arguments)?,
            "record_decision" => self.record_decision(arguments)?,
            "record_failed_approach" => self.record_failed_approach(arguments)?,
            "checkpoint_task" => self.checkpoint_task(arguments)?,
            "reconcile_project_knowledge" => self.reconcile_project_knowledge(arguments)?,
            "close_task" => self.close_task(arguments)?,
            "list_project_facts" => self.list_project_facts(arguments)?,
            "list_tasks" => self.list_tasks(arguments)?,
            "get_latest_reconciliation" => self.get_latest_reconciliation()?,
            _ => anyhow::bail!("未知 MCP 工具: {}", name),
        };

        text_result(payload)
    }

    fn get_project_status(&self) -> Result<Value> {
        let status = project_status(self.project_root.clone())?;
        Ok(json!({
            "project_root": status.project_root,
            "initialized": status.initialized,
            "config_exists": status.config_exists,
            "project_profile_exists": status.project_profile_exists,
            "project_doc_exists": status.project_doc_exists,
            "latest_run": status.latest_run,
            "inbox_exists": status.inbox_exists,
            "inbox_total": status.inbox_total,
            "inbox_pending": status.inbox_pending,
            "draft_total": status.draft_total,
            "draft_pending": status.draft_pending,
            "fact_patch_total": status.fact_patch_total,
            "fact_patch_pending": status.fact_patch_pending,
            "fact_patch_revertible": status.fact_patch_revertible,
            "index_exists": status.index_exists,
            "git_has_changes": status.git_has_changes,
            "suggested_next_steps": status.suggested_next_steps,
            "source_path": ".cyclaw"
        }))
    }

    fn get_project_profile(&self) -> Result<Value> {
        let path = self
            .project_root
            .join(".cyclaw")
            .join("project-profile.json");
        let content = fs::read_to_string(&path).with_context(|| {
            format!("无法读取项目画像，请先运行 cyclaw scan: {}", path.display())
        })?;
        let mut profile: Value = serde_json::from_str(&content)?;
        if let Some(object) = profile.as_object_mut() {
            object.insert(
                "source_path".to_string(),
                json!(".cyclaw/project-profile.json"),
            );
        }
        Ok(profile)
    }

    fn search_project_knowledge(&self, arguments: Value) -> Result<Value> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .context("缺少 query 参数")?;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let results = search_project(SearchOptions::new(
            self.project_root.clone(),
            query.to_string(),
            limit,
        ))?;

        Ok(json!({
            "query": query,
            "limit": limit,
            "results": results
        }))
    }

    fn list_pending_knowledge(&self) -> Result<Value> {
        let inbox = list_inbox(self.project_root.clone())?;
        let candidates = inbox
            .candidates
            .into_iter()
            .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
            .collect::<Vec<_>>();

        Ok(json!({
            "source_path": relative_path(&self.project_root, &inbox.inbox_path),
            "count": candidates.len(),
            "candidates": candidates
        }))
    }

    fn list_document_patches(&self) -> Result<Value> {
        let patches = list_document_patches(self.project_root.clone())?
            .into_iter()
            .filter(|patch| patch.status == DocumentPatchStatus::Pending)
            .collect::<Vec<_>>();

        Ok(json!({
            "source_path": ".cyclaw/doc-patches",
            "count": patches.len(),
            "patches": patches
        }))
    }

    fn get_policy(&self) -> Result<Value> {
        let policy = load_or_default(&self.project_root)?;
        Ok(json!({
            "schema_version": policy.schema_version,
            "permissions": policy.permissions,
            "model_policy": policy.model_policy,
            "automation": policy.automation,
            "source_path": ".cyclaw/config.yaml"
        }))
    }

    fn get_model_providers(&self) -> Result<Value> {
        let providers = list_providers(self.project_root.clone())?;
        Ok(json!({
            "active_provider": providers.active_provider,
            "providers": providers.providers,
            "source_path": relative_path(&self.project_root, &providers.config_path)
        }))
    }

    fn list_events(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let mut events = read_events(&self.project_root)?;
        events.reverse();
        events.truncate(limit);
        Ok(
            json!({ "count": events.len(), "events": events, "source_path": ".cyclaw/events.jsonl" }),
        )
    }

    fn list_agent_runs(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let dir = self.project_root.join(".cyclaw").join("agent-runs");
        let mut paths = Vec::new();
        if dir.exists() {
            for entry in fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) == Some("json") {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        paths.reverse();
        let mut runs = Vec::new();
        for path in paths.into_iter().take(limit) {
            let content = fs::read_to_string(&path)?;
            let mut run: Value = serde_json::from_str(&content)?;
            if let Some(object) = run.as_object_mut() {
                object.insert(
                    "source_path".to_string(),
                    json!(relative_path(&self.project_root, &path)),
                );
            }
            runs.push(run);
        }
        Ok(json!({ "count": runs.len(), "runs": runs, "source_path": ".cyclaw/agent-runs" }))
    }

    fn analyze_changes(&self) -> Result<Value> {
        let tick = watch_project_once(&self.project_root, None)?;
        let analyzed = tick
            .inbox_result
            .as_ref()
            .map(|result| result.generated.len())
            .unwrap_or_default();
        let added = tick
            .inbox_result
            .as_ref()
            .map(|result| result.added.len())
            .unwrap_or_default();
        Ok(json!({
            "changed": tick.changed,
            "run_id": tick.diff_result.as_ref().map(|result| result.run_id.clone()),
            "changed_files": tick.diff_result.as_ref().map(|result| &result.analysis.changed_files),
            "analyzed_candidates": analyzed,
            "added_candidates": added,
            "auto_applied_patches": tick.auto_applied_patches
        }))
    }

    fn get_candidate_detail(&self, arguments: Value) -> Result<Value> {
        let id = required_string(&arguments, "candidate_id")?;
        let candidate = list_inbox(self.project_root.clone())?
            .candidates
            .into_iter()
            .find(|candidate| candidate.id == id)
            .with_context(|| format!("未找到候选知识: {}", id))?;
        let patches = list_document_patches(self.project_root.clone())?
            .into_iter()
            .filter(|patch| patch.candidate_id == id)
            .collect::<Vec<_>>();
        Ok(json!({ "candidate": candidate, "patches": patches }))
    }

    fn preview_document_patch(&self, arguments: Value) -> Result<Value> {
        let id = required_string(&arguments, "candidate_id")?;
        let explicit_operation = arguments.get("operation").is_some();
        let existing = if explicit_operation {
            None
        } else {
            list_document_patches(self.project_root.clone())?
                .into_iter()
                .find(|patch| patch.candidate_id == id)
        };
        let patch = match existing {
            Some(patch) => patch,
            None => generate_document_drafts(self.draft_options(id, true, &arguments)?)?
                .patches
                .into_iter()
                .next()
                .context("未生成文档草稿")?,
        };
        Ok(json!({ "patch": patch, "target_document_modified": false }))
    }

    fn review_candidate(&self, arguments: Value) -> Result<Value> {
        let id = required_string(&arguments, "candidate_id")?;
        let action = required_string(&arguments, "action")?;
        let status = match action {
            "accept" => KnowledgeStatus::Accepted,
            "ignore" => KnowledgeStatus::Ignored,
            _ => anyhow::bail!("action 仅支持 accept 或 ignore"),
        };
        let result = update_inbox_status(self.project_root.clone(), id, status)?;
        let patch = if action == "accept"
            && arguments
                .get("generate_draft")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            generate_document_drafts(self.draft_options(id, false, &arguments)?)?
                .patches
                .into_iter()
                .next()
        } else {
            None
        };
        Ok(json!({ "candidate": result.candidate, "patch": patch }))
    }

    fn draft_options(
        &self,
        candidate_id: &str,
        include_pending: bool,
        arguments: &Value,
    ) -> Result<DraftOptions> {
        let mut options = DraftOptions::new(
            self.project_root.clone(),
            Some(candidate_id.to_string()),
            include_pending,
        );
        let Some(operation) = arguments.get("operation").and_then(Value::as_str) else {
            return Ok(options);
        };
        let selector = arguments
            .get("selector")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let source_selectors = arguments
            .get("source_selectors")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(ToString::to_string)
                            .context("source_selectors 必须全部为字符串")
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let replacement_content = arguments
            .get("replacement_content")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let delete_target_document = arguments
            .get("delete_target_document")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        options = options.with_operation(
            operation.parse::<KnowledgeOperation>()?,
            selector,
            source_selectors,
            replacement_content,
            delete_target_document,
        );
        Ok(options)
    }

    fn apply_document_patch(&self, arguments: Value) -> Result<Value> {
        let id = required_string(&arguments, "patch_id")?;
        let result = apply_patch(self.project_root.clone(), id)?;
        Ok(json!({
            "patch": result.patch,
            "target_doc": relative_path(&self.project_root, &result.target_doc_path),
            "revert_available": true
        }))
    }

    fn revert_document_patch(&self, arguments: Value) -> Result<Value> {
        let id = required_string(&arguments, "patch_id")?;
        let result = revert_patch(self.project_root.clone(), id)?;
        Ok(json!({
            "patch": result.patch,
            "target_doc": relative_path(&self.project_root, &result.target_doc_path),
            "reverted": true
        }))
    }

    fn list_fact_patches(&self, arguments: Value) -> Result<Value> {
        let page = core_query_fact_patches(
            &self.project_root,
            FactPatchQuery {
                status: arguments
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::parse::<FactPatchStatus>)
                    .transpose()?,
                operation: arguments
                    .get("operation")
                    .and_then(Value::as_str)
                    .map(str::parse::<FactOperation>)
                    .transpose()?,
                offset: optional_usize(&arguments, "offset", 0),
                limit: optional_usize(&arguments, "limit", 50),
            },
        )?;
        Ok(serde_json::to_value(page)?)
    }

    fn preview_fact_patch(&self, arguments: Value) -> Result<Value> {
        let operation = required_string(&arguments, "operation")?.parse::<FactOperation>()?;
        let fact = arguments
            .get("fact")
            .cloned()
            .map(serde_json::from_value::<ProjectFact>)
            .transpose()?;
        let patch = preview_fact_patch_core(
            &self.project_root,
            FactPatchRequest {
                operation,
                target_fact_id: arguments
                    .get("target_fact_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                source_fact_ids: optional_string_array(&arguments, "source_fact_ids")?,
                fact,
            },
        )?;
        Ok(json!({"patch":patch,"ledger_modified":false,"revert_available":false}))
    }

    fn apply_fact_patch(&self, arguments: Value) -> Result<Value> {
        let patch =
            apply_fact_patch_core(&self.project_root, required_string(&arguments, "patch_id")?)?;
        Ok(json!({"patch":patch,"revert_available":true}))
    }

    fn revert_fact_patch(&self, arguments: Value) -> Result<Value> {
        let patch =
            revert_fact_patch_core(&self.project_root, required_string(&arguments, "patch_id")?)?;
        Ok(json!({"patch":patch,"reverted":true}))
    }

    fn verify_fact_evidence(&self, arguments: Value) -> Result<Value> {
        Ok(json!({"report": verify_fact_evidence_core(
            &self.project_root,
            required_string(&arguments, "fact_id")?,
        )?}))
    }

    fn list_evidence_verifications(&self, arguments: Value) -> Result<Value> {
        let page = core_query_evidence_verifications(
            &self.project_root,
            arguments.get("fact_id").and_then(Value::as_str),
            optional_usize(&arguments, "offset", 0),
            optional_usize(&arguments, "limit", 50),
        )?;
        Ok(serde_json::to_value(page)?)
    }

    fn list_fact_transactions(&self) -> Result<Value> {
        Ok(serde_json::to_value(core_diagnose_fact_transactions(
            &self.project_root,
        )?)?)
    }

    fn fact_operation(&self, operation: FactOperation, arguments: Value) -> Result<Value> {
        let target_fact_id = arguments
            .get("target_fact_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let source_fact_ids = optional_string_array(&arguments, "source_fact_ids")?;
        let fact = self.fact_from_shortcut(&operation, target_fact_id.as_deref(), &arguments)?;
        let preview = preview_fact_patch_core(
            &self.project_root,
            FactPatchRequest {
                operation,
                target_fact_id,
                source_fact_ids,
                fact,
            },
        )?;
        if arguments
            .get("apply")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let patch = apply_fact_patch_core(&self.project_root, &preview.id)?;
            return Ok(json!({"patch":patch,"ledger_modified":true,"revert_available":true}));
        }
        Ok(json!({"patch":preview,"ledger_modified":false,"revert_available":false}))
    }

    fn fact_from_shortcut(
        &self,
        operation: &FactOperation,
        target_fact_id: Option<&str>,
        arguments: &Value,
    ) -> Result<Option<ProjectFact>> {
        if *operation == FactOperation::Delete {
            return Ok(None);
        }
        let has_overrides = [
            "statement",
            "fact_type",
            "evidence",
            "evidence_details",
            "confidence",
        ]
        .iter()
        .any(|name| arguments.get(name).is_some());
        if *operation == FactOperation::Update && !has_overrides {
            anyhow::bail!("update_fact 至少需要提供一个修改字段");
        }
        if *operation == FactOperation::Merge && !has_overrides {
            return Ok(None);
        }
        let mut fact = if let Some(id) = target_fact_id {
            core_list_project_facts(&self.project_root)?
                .into_iter()
                .find(|fact| fact.id == id)
                .with_context(|| format!("未找到事实: {}", id))?
        } else {
            project_fact_from_input(FactInput {
                statement: String::new(),
                fact_type: FactType::Unknown,
                evidence: Vec::new(),
                evidence_details: Vec::new(),
                source_task_id: None,
                confidence: 90,
                valid_from: None,
                valid_until: None,
            })
        };
        if let Some(statement) = arguments.get("statement").and_then(Value::as_str) {
            fact.statement = statement.to_string();
        }
        if matches!(operation, FactOperation::Create | FactOperation::Supersede)
            && fact.statement.trim().is_empty()
        {
            anyhow::bail!("create_fact 和 supersede_fact 必须提供 statement");
        }
        if let Some(fact_type) = arguments.get("fact_type").and_then(Value::as_str) {
            fact.fact_type = fact_type.parse::<FactType>()?;
        }
        if arguments.get("evidence").is_some() {
            fact.evidence = optional_string_array(arguments, "evidence")?;
            fact.evidence_details.clear();
        }
        if let Some(details) = arguments.get("evidence_details") {
            fact.evidence_details = serde_json::from_value::<Vec<FactEvidence>>(details.clone())?;
        }
        if let Some(confidence) = arguments.get("confidence").and_then(Value::as_u64) {
            fact.confidence = confidence.min(100) as u8;
        }
        if matches!(operation, FactOperation::Create | FactOperation::Supersede) {
            fact.id.clear();
            fact.supersedes.clear();
        }
        Ok(Some(fact))
    }

    fn run_agent(&self, arguments: Value) -> Result<Value> {
        let use_model = arguments
            .get("use_model")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let provider = arguments
            .get("provider")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let result = run_agent_once(AgentRunOptions::new(
            self.project_root.clone(),
            provider,
            use_model,
        ))?;
        Ok(
            json!({ "record": result.record, "record_path": relative_path(&self.project_root, &result.record_path) }),
        )
    }

    fn set_runtime_strategy(&self, arguments: Value) -> Result<Value> {
        let strategy = required_string(&arguments, "strategy")?;
        let (docs, model, auto) = match strategy {
            "observe" => (false, false, false),
            "review" => (true, false, false),
            "smart" => (true, true, false),
            "auto" => (true, true, true),
            _ => anyhow::bail!("未知运行策略: {}", strategy),
        };
        set_permission(&self.project_root, "allow_docs_apply", docs)?;
        set_permission(&self.project_root, "allow_model_call", model)?;
        set_permission(&self.project_root, "allow_network", model)?;
        let mut policy = set_permission(&self.project_root, "allow_auto_apply_docs", auto)?;
        if let Some(confidence) = arguments
            .get("auto_apply_min_confidence")
            .and_then(Value::as_u64)
        {
            policy = set_auto_apply_min_confidence(&self.project_root, confidence.min(100) as u8)?;
        }
        Ok(json!({ "strategy": strategy, "policy": policy }))
    }

    fn doctor(&self) -> Result<Value> {
        let status = project_status(self.project_root.clone())?;
        let policy = load_or_default(&self.project_root)?;
        let providers = list_providers(self.project_root.clone())?;
        let transactions = core_diagnose_fact_transactions(&self.project_root)?;
        let recovery = core_get_latest_fact_recovery(&self.project_root)?;
        let required_checks = vec![
            json!({"name":"git_repository","ok":self.project_root.join(".git").exists(),"severity":"required"}),
            json!({"name":"cyclaw_initialized","ok":status.initialized,"severity":"required"}),
            json!({"name":"policy_loaded","ok":status.config_exists,"severity":"required"}),
            json!({"name":"knowledge_inbox","ok":status.inbox_exists,"severity":"required"}),
            json!({
                "name":"fact_transactions",
                "ok":transactions.blocked_count == 0,
                "severity":"required",
                "pending":transactions.pending_count,
                "recoverable":transactions.recoverable_count,
                "blocked":transactions.blocked_count,
                "details":transactions.transactions
            }),
        ];
        let optional_checks = vec![
            json!({"name":"active_model","ok":providers.active_provider.is_some(),"severity":"optional"}),
            json!({"name":"docs_write_permission","ok":policy.permissions.allow_docs_apply,"severity":"optional"}),
        ];
        let healthy = required_checks
            .iter()
            .all(|check| check["ok"].as_bool().unwrap_or(false));
        let degraded_capabilities = optional_checks
            .iter()
            .filter(|check| !check["ok"].as_bool().unwrap_or(false))
            .filter_map(|check| check["name"].as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        let checks = required_checks
            .into_iter()
            .chain(optional_checks)
            .collect::<Vec<_>>();
        Ok(json!({
            "healthy": healthy,
            "checks": checks,
            "degraded_capabilities": degraded_capabilities,
            "latest_fact_recovery": recovery,
            "suggested_next_steps": status.suggested_next_steps
        }))
    }

    fn begin_task(&self, arguments: Value) -> Result<Value> {
        let title = required_string(&arguments, "title")?.to_string();
        let objective = required_string(&arguments, "objective")?.to_string();
        let mut options = BeginTaskOptions::new(self.project_root.clone(), title, objective);
        options.related_files = optional_string_array(&arguments, "related_files")?;
        options.context_budget_tokens = arguments
            .get("context_budget_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(2_000)
            .clamp(256, 16_000) as usize;
        let task = begin_project_task(options)?;
        let context = core_get_task_context(
            &self.project_root,
            Some(&task.id),
            None,
            Some(task.context_budget_tokens),
            20,
        )?;
        Ok(json!({"task":task,"context":context}))
    }

    fn get_active_task(&self) -> Result<Value> {
        Ok(json!({"task":core_get_active_task(&self.project_root)?}))
    }

    fn get_task_context(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let query = arguments.get("query").and_then(Value::as_str);
        let budget = arguments
            .get("budget_tokens")
            .and_then(Value::as_u64)
            .map(|value| value.clamp(256, 16_000) as usize);
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        Ok(json!(core_get_task_context(
            &self.project_root,
            task_id,
            query,
            budget,
            limit,
        )?))
    }

    fn record_decision(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let statement = required_string(&arguments, "statement")?.to_string();
        let rationale = required_string(&arguments, "rationale")?.to_string();
        let evidence = optional_string_array(&arguments, "evidence")?;
        let confidence = arguments
            .get("confidence")
            .and_then(Value::as_u64)
            .unwrap_or(90)
            .min(100) as u8;
        let (task, fact) = record_task_decision(
            &self.project_root,
            task_id,
            statement,
            rationale,
            evidence,
            confidence,
        )?;
        Ok(json!({"task":task,"fact":fact}))
    }

    fn record_failed_approach(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let approach = required_string(&arguments, "approach")?.to_string();
        let reason = required_string(&arguments, "reason")?.to_string();
        let evidence = optional_string_array(&arguments, "evidence")?;
        let (task, fact) =
            record_task_failed_approach(&self.project_root, task_id, approach, reason, evidence)?;
        Ok(json!({"task":task,"fact":fact}))
    }

    fn checkpoint_task(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let summary = required_string(&arguments, "summary")?.to_string();
        let related_files = optional_string_array(&arguments, "related_files")?;
        Ok(json!({"task":checkpoint_project_task(
            &self.project_root,
            task_id,
            summary,
            related_files,
        )?}))
    }

    fn reconcile_project_knowledge(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments
            .get("task_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                core_get_active_task(&self.project_root)
                    .ok()
                    .flatten()
                    .map(|task| task.id)
            });
        Ok(json!({"report":core_reconcile_project_knowledge(
            &self.project_root,
            task_id,
        )?}))
    }

    fn close_task(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let summary = required_string(&arguments, "summary")?.to_string();
        let reconcile = arguments
            .get("reconcile")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(json!(close_project_task(
            &self.project_root,
            task_id,
            summary,
            reconcile,
        )?))
    }

    fn list_project_facts(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 200) as usize;
        let mut facts = core_list_project_facts(&self.project_root)?;
        facts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        facts.truncate(limit);
        Ok(json!({"count":facts.len(),"facts":facts,"source_path":".cyclaw/memory/facts.jsonl"}))
    }

    fn list_tasks(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let tasks = core_list_tasks(&self.project_root, limit)?;
        Ok(json!({"count":tasks.len(),"tasks":tasks,"source_path":".cyclaw/tasks"}))
    }

    fn get_latest_reconciliation(&self) -> Result<Value> {
        Ok(json!({"report":core_get_latest_reconciliation(&self.project_root)?}))
    }

    fn list_resources(&self) -> Result<Value> {
        let mut resources = Vec::new();

        for (path, name, mime_type) in [
            (".cyclaw/project.md", "cyClaw 项目说明", "text/markdown"),
            (
                ".cyclaw/knowledge-inbox.jsonl",
                "cyClaw 知识收件箱",
                "application/jsonl",
            ),
        ] {
            if self.project_root.join(path).exists() {
                resources.push(resource(path, name, mime_type));
            }
        }

        collect_resource_files(
            &self.project_root,
            &self.project_root.join(".cyclaw").join("doc-patches"),
            "cyClaw 文档草稿",
            "application/json",
            &mut resources,
        )?;
        collect_resource_files(
            &self.project_root,
            &self.project_root.join("docs"),
            "项目文档",
            "text/markdown",
            &mut resources,
        )?;
        collect_resource_files(
            &self.project_root,
            &self.project_root.join(".cyclaw").join("memory"),
            "cyClaw 结构化项目事实",
            "application/jsonl",
            &mut resources,
        )?;
        collect_resource_files(
            &self.project_root,
            &self.project_root.join(".cyclaw").join("tasks"),
            "cyClaw 项目任务记忆",
            "application/json",
            &mut resources,
        )?;
        collect_resource_files(
            &self.project_root,
            &self.project_root.join(".cyclaw").join("reconciliation"),
            "cyClaw 知识对账报告",
            "application/json",
            &mut resources,
        )?;

        Ok(json!({ "resources": resources }))
    }

    fn read_resource(&self, params: Value) -> Result<Value> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .context("缺少资源 uri")?;
        let relative = uri
            .strip_prefix("cyclaw://")
            .context("资源 uri 必须以 cyclaw:// 开头")?;
        let path = safe_read_path(&self.project_root, relative)?;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("无法读取 MCP 资源: {}", path.display()))?;

        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": mime_type_for(relative),
                "text": text
            }]
        }))
    }
}

struct McpTransport<R> {
    reader: BufReader<R>,
}

impl<R> McpTransport<R>
where
    R: Read,
{
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }

    fn next_message(&mut self) -> Result<Option<Value>> {
        let mut content_length = None;
        let mut line = String::new();

        loop {
            line.clear();
            let bytes = self.reader.read_line(&mut line)?;
            if bytes == 0 {
                return Ok(None);
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }

            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }

        let length = content_length.context("MCP 消息缺少 Content-Length")?;
        let mut body = vec![0; length];
        self.reader.read_exact(&mut body)?;
        Ok(Some(serde_json::from_slice(&body)?))
    }
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn text_result<T: Serialize>(payload: T) -> Result<Value> {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload)?
        }]
    }))
}

fn object_schema(properties: Vec<(&str, Value)>) -> Value {
    let required = properties
        .iter()
        .filter_map(|(name, _)| (*name == "query").then_some(*name))
        .collect::<Vec<_>>();
    object_schema_with_required(properties, &required)
}

fn object_schema_with_required(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut property_map = serde_json::Map::new();

    for (name, schema) in properties {
        property_map.insert(name.to_string(), schema);
    }

    json!({
        "type": "object",
        "properties": property_map,
        "required": required,
        "additionalProperties": false
    })
}

fn fact_operation_tool(name: &str, description: &str, target: bool, sources: bool) -> Value {
    let mut properties = Vec::new();
    if name != "delete_fact" {
        properties.extend([
            ("statement", json!({"type":"string"})),
            (
                "fact_type",
                json!({"type":"string","enum":["decision","failed_approach","constraint","api_contract","schema_rule","dependency","environment","architecture","operational","unknown"]}),
            ),
            (
                "evidence",
                json!({"type":"array","items":{"type":"string"}}),
            ),
            (
                "evidence_details",
                json!({"type":"array","items":fact_evidence_schema()}),
            ),
            (
                "confidence",
                json!({"type":"integer","minimum":0,"maximum":100}),
            ),
        ]);
    }
    properties.push((
        "apply",
        json!({"type":"boolean","description":"为 true 时在生成预览后立即应用；默认只生成草稿。"}),
    ));
    if target {
        properties.push(("target_fact_id", json!({"type":"string"})));
    }
    if sources {
        properties.push((
            "source_fact_ids",
            json!({"type":"array","items":{"type":"string"}}),
        ));
    }
    let required = match name {
        "create_fact" => vec!["statement"],
        "update_fact" | "delete_fact" => vec!["target_fact_id"],
        "merge_facts" => vec!["target_fact_id", "source_fact_ids"],
        "supersede_fact" => vec!["target_fact_id", "statement"],
        _ => Vec::new(),
    };
    json!({"name":name,"description":description,"inputSchema":object_schema_with_required(properties, &required)})
}

fn fact_evidence_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "path":{"type":"string"},
            "symbol":{"type":"string"},
            "line_start":{"type":"integer","minimum":1},
            "line_end":{"type":"integer","minimum":1},
            "content_hash":{"type":"string"},
            "hash_scope":{"type":"string","enum":["file","line_range","symbol"]},
            "git_head":{"type":"string"},
            "captured_at":{"type":"string"},
            "verified_at":{"type":"string"},
            "evidence_type":{"type":"string"}
        },
        "required":["path"],
        "additionalProperties":false
    })
}

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("缺少字符串参数: {}", name))
}

fn optional_usize(arguments: &Value, name: &str, default: usize) -> usize {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn optional_string_array(arguments: &Value, name: &str) -> Result<Vec<String>> {
    arguments
        .get(name)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToString::to_string)
                        .with_context(|| format!("{} 必须全部为字符串", name))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "analyze_changes"
            | "preview_document_patch"
            | "review_candidate"
            | "apply_document_patch"
            | "revert_document_patch"
            | "preview_fact_patch"
            | "apply_fact_patch"
            | "revert_fact_patch"
            | "verify_fact_evidence"
            | "create_fact"
            | "update_fact"
            | "merge_facts"
            | "supersede_fact"
            | "delete_fact"
            | "run_agent"
            | "set_runtime_strategy"
            | "begin_task"
            | "record_decision"
            | "record_failed_approach"
            | "checkpoint_task"
            | "reconcile_project_knowledge"
            | "close_task"
    )
}

fn resource(path: &str, name: &str, mime_type: &str) -> Value {
    json!({
        "uri": format!("cyclaw://{}", path),
        "name": name,
        "mimeType": mime_type
    })
}

fn collect_resource_files(
    project_root: &Path,
    dir: &Path,
    label: &str,
    mime_type: &str,
    resources: &mut Vec<Value>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in
        fs::read_dir(dir).with_context(|| format!("无法读取资源目录: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_resource_files(project_root, &path, label, mime_type, resources)?;
            continue;
        }

        let is_supported = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("md") | Some("json") | Some("jsonl")
        );
        if !is_supported {
            continue;
        }

        let relative = relative_path(project_root, &path);
        resources.push(resource(
            &relative,
            &format!("{}: {}", label, relative),
            mime_type,
        ));
    }

    Ok(())
}

fn safe_read_path(project_root: &Path, relative: &str) -> Result<PathBuf> {
    let normalized = relative.replace('\\', "/");
    let allowed = normalized == ".cyclaw/project.md"
        || normalized == ".cyclaw/knowledge-inbox.jsonl"
        || normalized.starts_with(".cyclaw/doc-patches/")
        || normalized.starts_with(".cyclaw/memory/")
        || normalized.starts_with(".cyclaw/tasks/")
        || normalized.starts_with(".cyclaw/reconciliation/")
        || normalized.starts_with("docs/");

    if !allowed || normalized.contains("..") {
        anyhow::bail!("不允许读取 MCP 资源路径: {}", relative);
    }

    Ok(project_root.join(normalized))
}

fn mime_type_for(path: &str) -> &'static str {
    if path.ends_with(".md") {
        "text/markdown"
    } else if path.ends_with(".jsonl") {
        "application/jsonl"
    } else {
        "application/json"
    }
}

fn relative_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|relative| relative.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cyclaw_model::{AddProviderOptions, add_provider};

    #[test]
    fn handles_initialize_and_tools_list() {
        let temp = tempfile::tempdir().unwrap();
        let input = framed(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })) + &framed(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }));

        let mut output = Vec::new();
        run_server(
            McpServerOptions::new(temp.path().to_path_buf()),
            input.as_bytes(),
            &mut output,
        )
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("cyclaw"));
        assert!(text.contains("search_project_knowledge"));
        assert!(text.contains("get_model_providers"));
        assert!(text.contains("analyze_changes"));
        assert!(text.contains("apply_document_patch"));
        assert!(text.contains("supersede"));
        assert!(text.contains("begin_task"));
        assert!(text.contains("record_failed_approach"));
        assert!(text.contains("reconcile_project_knowledge"));
        assert!(text.contains("list_evidence_verifications"));
        assert!(text.contains("list_fact_transactions"));
        assert!(text.contains("doctor"));
    }

    #[test]
    fn rejects_unsafe_resource_path() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let result = server.read_resource(json!({ "uri": "cyclaw://../Cargo.toml" }));

        assert!(result.is_err());
    }

    #[test]
    fn reads_model_providers_without_api_key() {
        let temp = tempfile::tempdir().unwrap();
        add_provider(AddProviderOptions {
            project_root: temp.path().to_path_buf(),
            name: "test".to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "test-model".to_string(),
            api_key_env: "CYCLAW_TEST_KEY".to_string(),
            thinking_enabled: false,
            set_active: true,
        })
        .unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let result = server.get_model_providers().unwrap();

        assert_eq!(result["active_provider"], "test");
        assert_eq!(
            result["providers"]["test"]["api_key_env"],
            "CYCLAW_TEST_KEY"
        );
        assert!(result.to_string().find("sk-").is_none());
    }

    #[test]
    fn doctor_reports_uninitialized_project() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let result = server.doctor().unwrap();

        assert_eq!(result["healthy"], false);
        assert!(result["checks"].is_array());
        assert!(result["latest_fact_recovery"].is_null());
        assert!(
            result["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["severity"] == "required")
        );
    }

    #[test]
    fn doctor_keeps_required_health_when_optional_capabilities_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        cyclaw_core::init_project(cyclaw_core::InitOptions::new(temp.path().to_path_buf()))
            .unwrap();
        fs::write(temp.path().join(".cyclaw/knowledge-inbox.jsonl"), "").unwrap();
        let server = McpServer::new(temp.path().to_path_buf());

        let result = server.doctor().unwrap();

        assert_eq!(result["healthy"], true);
        let degraded = result["degraded_capabilities"].as_array().unwrap();
        assert!(degraded.iter().any(|item| item == "active_model"));
        assert!(degraded.iter().any(|item| item == "docs_write_permission"));
        let optional_checks = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|check| check["severity"] == "optional")
            .count();
        assert_eq!(optional_checks, 2);
    }

    #[test]
    fn doctor_reports_blocked_fact_transaction_without_preventing_startup() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".cyclaw/memory/fact-transactions");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("fact_patch_blocked.json"),
            r#"{"patch_id":"fact_patch_blocked","operation":"apply","started_at":"2026-08-04T00:00:00Z","before_fingerprint":"before","after_fingerprint":"after"}"#,
        )
        .unwrap();
        let server = McpServer::new(temp.path().to_path_buf());

        let result = server.doctor().unwrap();
        let transaction_check = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "fact_transactions")
            .unwrap();
        assert_eq!(transaction_check["ok"], false);
        assert_eq!(transaction_check["blocked"], 1);
        assert_eq!(transaction_check["severity"], "required");

        let input = framed(json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{}
        }));
        let mut output = Vec::new();
        run_server(
            McpServerOptions::new(temp.path().to_path_buf()),
            input.as_bytes(),
            &mut output,
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("protocolVersion")
        );
    }

    #[test]
    fn task_protocol_records_and_recalls_decision() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let started = server
            .begin_task(json!({
                "title":"退款状态机",
                "objective":"调整退款处理中状态",
                "context_budget_tokens":1000
            }))
            .unwrap();
        let task_id = started["task"]["id"].as_str().unwrap();
        server
            .record_decision(json!({
                "task_id":task_id,
                "statement":"退款完成后不得重新进入处理中",
                "rationale":"避免重复退款",
                "evidence":[],
                "confidence":95
            }))
            .unwrap();
        let context = server
            .get_task_context(json!({"task_id":task_id,"query":"退款处理中"}))
            .unwrap();

        assert!(context.to_string().contains("不得重新进入处理中"));
    }

    #[test]
    fn fact_patch_protocol_previews_applies_and_reverts() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let preview = server.preview_fact_patch(json!({
            "operation":"create",
            "fact":{"id":"","statement":"测试事实","fact_type":"constraint","status":"active","evidence":["src/lib.rs"],"confidence":90,"supersedes":[],"created_at":"","updated_at":"","last_verified_at":""}
        })).unwrap();
        let id = preview["patch"]["id"].as_str().unwrap();
        let applied = server.apply_fact_patch(json!({"patch_id":id})).unwrap();
        assert_eq!(applied["patch"]["status"], "applied");
        let reverted = server.revert_fact_patch(json!({"patch_id":id})).unwrap();
        assert_eq!(reverted["patch"]["status"], "reverted");
    }

    #[test]
    fn ergonomic_fact_tool_applies_and_verifies_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let created = server
            .fact_operation(
                FactOperation::Create,
                json!({
                    "statement":"MCP 事实操作必须生成草稿",
                    "fact_type":"constraint",
                    "evidence":["missing.rs"],
                    "confidence":95,
                    "apply":true
                }),
            )
            .unwrap();
        assert_eq!(created["patch"]["status"], "applied");
        let fact_id = created["patch"]["after"][0]["id"].as_str().unwrap();
        let verification = server
            .verify_fact_evidence(json!({"fact_id":fact_id}))
            .unwrap();
        assert_eq!(verification["report"]["issue_count"], 1);
        assert_eq!(verification["report"]["results"][0]["status"], "missing");
        let history = server
            .list_evidence_verifications(json!({"fact_id":fact_id,"limit":10}))
            .unwrap();
        assert_eq!(history["records"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn fact_tool_schemas_declare_operation_specific_required_fields() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::new(temp.path().to_path_buf());
        let tools = server.tools();
        let required = |name: &str| {
            tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
        };

        assert_eq!(required("create_fact"), vec!["statement"]);
        assert_eq!(required("update_fact"), vec!["target_fact_id"]);
        assert_eq!(
            required("merge_facts"),
            vec!["target_fact_id", "source_fact_ids"]
        );
        assert_eq!(
            required("supersede_fact"),
            vec!["target_fact_id", "statement"]
        );
        assert_eq!(required("delete_fact"), vec!["target_fact_id"]);
    }

    fn framed(value: Value) -> String {
        let body = serde_json::to_string(&value).unwrap();
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
    }
}
