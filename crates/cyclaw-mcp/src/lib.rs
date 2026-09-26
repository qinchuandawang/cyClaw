use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cyclaw_agent::{
    AgentRunOptions, RetryRunOptions, abandon_retry_task, run_agent_once, run_due_retries,
};
use cyclaw_core::TaskPhase;
use cyclaw_core::{
    BeginTaskOptions, DraftOptions, FactEvidence, FactInput, FactOperation, FactPatchQuery,
    FactPatchRequest, FactPatchStatus, FactType, ProjectFact, SearchOptions,
    apply_document_patch as apply_patch, apply_fact_patch as apply_fact_patch_core,
    begin_task as begin_project_task,
    checkpoint_task_for_session as checkpoint_project_task_for_session,
    close_task_for_session as close_project_task_for_session,
    diagnose_fact_patch_transactions as core_diagnose_fact_transactions, generate_document_drafts,
    get_active_task_for_session as core_get_active_task_for_session,
    get_latest_fact_recovery_report as core_get_latest_fact_recovery,
    get_latest_reconciliation as core_get_latest_reconciliation,
    get_task_context_for_session as core_get_task_context_for_session, list_document_patches,
    list_inbox, list_project_facts as core_list_project_facts, list_tasks as core_list_tasks,
    observer_health, preview_fact_patch as preview_fact_patch_core, project_fact_from_input,
    project_status, query_evidence_verifications as core_query_evidence_verifications,
    query_fact_patches as core_query_fact_patches,
    reconcile_project_knowledge as core_reconcile_project_knowledge, record_execution_event,
    record_task_decision_for_session, record_task_failed_approach_for_session,
    recover_fact_patch_transactions as recover_fact_transactions,
    revert_document_patch as revert_patch, revert_fact_patch as revert_fact_patch_core,
    search_project, set_task_phase_for_session as set_project_task_phase_for_session,
    update_inbox_status, verify_fact_evidence as verify_fact_evidence_core, watch_project_once,
};
use cyclaw_docs::{DocumentPatchStatus, KnowledgeOperation};
use cyclaw_events::{
    ExecutionEventKind, ModelCallRecord, RetryStatus, new_execution_event_with_context,
    read_events, read_execution_events, read_model_calls, read_retry_queue, read_trace_spans,
};
use cyclaw_knowledge::KnowledgeStatus;
use cyclaw_model::{list_providers, read_model_usage};
use cyclaw_policy::{load_or_default, set_permission};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct McpServerOptions {
    pub project_root: PathBuf,
    pub recover_transactions_on_startup: bool,
}

impl McpServerOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            recover_transactions_on_startup: false,
        }
    }

    pub fn with_transaction_recovery(mut self) -> Self {
        self.recover_transactions_on_startup = true;
        self
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
    if options.recover_transactions_on_startup {
        recover_fact_transactions(&options.project_root)?;
    }
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
                "name": "get_model_usage",
                "description": "读取当天模型调用次数、Token 用量和预算相关指标。",
                "inputSchema": object_schema(vec![])
            }),
            json!({
                "name": "list_events",
                "description": "读取 cyClaw 事件日志，帮助模型理解知识资产的演进过程。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100 }))])
            }),
            json!({
                "name": "list_traces",
                "description": "读取本地链路追踪索引：按 trace_id 聚合 span 数量、错误数与总耗时。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100 }))])
            }),
            json!({
                "name": "get_trace",
                "description": "按 trace_id 还原完整调用链，包含 span 树、大模型推理路径和关联执行事件；Agent 运行记录的 run_id 即其 trace_id。",
                "inputSchema": object_schema_with_required(
                    vec![("trace_id", json!({ "type": "string" }))],
                    &["trace_id"]
                )
            }),
            json!({
                "name": "list_model_calls",
                "description": "读取大模型推理调用明细：provider、token 用量、延迟、重试次数、缓存命中与脱敏输入输出预览。",
                "inputSchema": object_schema(vec![
                    ("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100 })),
                    ("provider", json!({ "type": "string" }))
                ])
            }),
            json!({
                "name": "list_retries",
                "description": "读取失败任务重试队列：状态、尝试次数、退避计划与降级原因。",
                "inputSchema": object_schema(vec![
                    ("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100 })),
                    ("status", json!({ "type": "string", "enum": ["pending", "in_progress", "completed", "exhausted", "abandoned"] }))
                ])
            }),
            json!({
                "name": "run_retries",
                "description": "立即执行所有到期重试任务：恢复失败上下文后重新运行模型审查，失败按指数退避重排，耗尽后降级为人工处理。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 50 }))])
            }),
            json!({
                "name": "abandon_retry",
                "description": "人工放弃一条重试任务（确认是逻辑错误而非瞬态故障），降级为人工处理。",
                "inputSchema": object_schema_with_required(
                    vec![("retry_id", json!({ "type": "string" }))],
                    &["retry_id"]
                )
            }),
            json!({
                "name": "list_agent_runs",
                "description": "读取最近 Agent 运行记录。",
                "inputSchema": object_schema(vec![("limit", json!({ "type": "integer", "minimum": 1, "maximum": 50 }))])
            }),
            json!({"name":"analyze_changes","description":"执行一次增量知识分析，生成新的候选知识。","inputSchema":object_schema(vec![])}),
            json!({"name":"record_execution_event","description":"记录构建、测试、命令或 Patch 的执行结果；失败仅生成待审知识候选，不直接写入 Fact。可传 trace_id/session_id 关联调用链。","inputSchema":object_schema_with_required(vec![("kind",json!({"type":"string","enum":["command","build","test","patch"]})),("command_summary",json!({"type":"string","maxLength":500})),("exit_code",json!({"type":"integer"})),("timed_out",json!({"type":"boolean"})),("error_summary",json!({"type":"string","maxLength":2000})),("related_files",json!({"type":"array","items":{"type":"string"},"maxItems":50})),("session_id",json!({"type":"string"})),("trace_id",json!({"type":"string"})),("duration_millis",json!({"type":"integer","minimum":0}))], &["kind","command_summary"])}),
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
            json!({"name":"set_runtime_strategy","description":"设置观察、人工审阅或模型辅助审阅策略；不会自动写入项目文档。","inputSchema":object_schema(vec![("strategy",json!({"type":"string","enum":["observe","review","smart"]}))])}),
            json!({"name":"doctor","description":"诊断 Git、配置、模型、权限、知识目录和文档写入状态。","inputSchema":object_schema(vec![])}),
            json!({"name":"begin_task","description":"按 session 开始一个项目任务，并返回首个带证据的上下文包。","inputSchema":object_schema(vec![("title",json!({"type":"string"})),("objective",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("related_files",json!({"type":"array","items":{"type":"string"}})),("context_budget_tokens",json!({"type":"integer","minimum":256,"maximum":16000}))])}),
            json!({"name":"get_active_task","description":"按 session 读取当前活动任务；没有活动任务时返回 null。","inputSchema":object_schema(vec![("session_id",json!({"type":"string","maxLength":128}))])}),
            json!({"name":"get_task_context","description":"按 session、查询和预算编译项目级最小上下文包。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("query",json!({"type":"string"})),("budget_tokens",json!({"type":"integer","minimum":256,"maximum":16000})),("limit",json!({"type":"integer","minimum":1,"maximum":50}))])}),
            json!({"name":"get_observer_health","description":"读取独立 Observer 的最近成功时间、错误和状态文件健康状态。","inputSchema":object_schema(vec![])}),
            json!({"name":"record_decision","description":"把任务中的关键决策写入任务记录和结构化 Fact Ledger。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("statement",json!({"type":"string"})),("rationale",json!({"type":"string"})),("evidence",json!({"type":"array","items":{"type":"string"}})),("confidence",json!({"type":"integer","minimum":0,"maximum":100}))])}),
            json!({"name":"record_failed_approach","description":"记录尝试过但失败的方案、原因和证据，供后续会话避免重复。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("approach",json!({"type":"string"})),("reason",json!({"type":"string"})),("evidence",json!({"type":"array","items":{"type":"string"}}))])}),
            json!({"name":"checkpoint_task","description":"按 session 记录长任务检查点、当前结论和相关文件。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("summary",json!({"type":"string"})),("related_files",json!({"type":"array","items":{"type":"string"}}))])}),
            json!({"name":"set_task_phase","description":"切换当前任务阶段。","inputSchema":object_schema_with_required(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("phase",json!({"type":"string","enum":["investigate","design","implement","verify","handoff"]}))], &["phase"])}),
            json!({"name":"reconcile_project_knowledge","description":"检测结构化事实中的重复、冲突、证据漂移和失效，并推荐 merge/supersede/update/delete。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128}))])}),
            json!({"name":"close_task","description":"按 session 关闭当前任务，可同时执行知识对账并返回交接信息。","inputSchema":object_schema(vec![("task_id",json!({"type":"string"})),("session_id",json!({"type":"string","maxLength":128})),("summary",json!({"type":"string"})),("reconcile",json!({"type":"boolean"}))])}),
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
                    | "get_model_usage"
                    | "list_events"
                    | "list_traces"
                    | "get_trace"
                    | "list_model_calls"
                    | "list_retries"
                    | "list_agent_runs"
                    | "get_candidate_detail"
                    | "list_fact_patches"
                    | "list_evidence_verifications"
                    | "list_fact_transactions"
                    | "doctor"
                    | "get_observer_health"
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
            "get_model_usage" => self.get_model_usage()?,
            "list_events" => self.list_events(arguments)?,
            "list_traces" => self.list_traces(arguments)?,
            "get_trace" => self.get_trace(arguments)?,
            "list_model_calls" => self.list_model_calls(arguments)?,
            "list_retries" => self.list_retries(arguments)?,
            "run_retries" => self.run_retries(arguments)?,
            "abandon_retry" => self.abandon_retry(arguments)?,
            "list_agent_runs" => self.list_agent_runs(arguments)?,
            "analyze_changes" => self.analyze_changes()?,
            "record_execution_event" => self.record_execution_event(arguments)?,
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
            "get_observer_health" => json!(observer_health(&self.project_root)?),
            "begin_task" => self.begin_task(arguments)?,
            "get_active_task" => self.get_active_task(arguments)?,
            "get_task_context" => self.get_task_context(arguments)?,
            "record_decision" => self.record_decision(arguments)?,
            "record_failed_approach" => self.record_failed_approach(arguments)?,
            "checkpoint_task" => self.checkpoint_task(arguments)?,
            "set_task_phase" => self.set_task_phase(arguments)?,
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

    fn get_model_usage(&self) -> Result<Value> {
        let policy = load_or_default(&self.project_root)?;
        Ok(json!({
            "usage": read_model_usage(&self.project_root),
            "daily_token_budget": policy.model_policy.daily_token_budget,
            "source_path": ".cyclaw/model-usage.json"
        }))
    }

    /// 按 trace_id 聚合 span，输出最近链路索引。
    fn list_traces(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let mut grouped: std::collections::BTreeMap<String, (String, usize, usize, u128)> =
            std::collections::BTreeMap::new();
        for span in read_trace_spans(&self.project_root)? {
            let entry = grouped
                .entry(span.trace_id.clone())
                .or_insert_with(|| (span.started_at.clone(), 0, 0, 0));
            entry.1 += 1;
            if span.status == cyclaw_events::TraceSpanStatus::Error {
                entry.2 += 1;
            }
            entry.3 = entry.3.saturating_add(span.duration_millis);
        }
        let mut traces: Vec<Value> = grouped
            .into_iter()
            .map(
                |(trace_id, (started_at, span_count, error_count, total_duration_millis))| {
                    json!({
                        "trace_id": trace_id,
                        "started_at": started_at,
                        "span_count": span_count,
                        "error_count": error_count,
                        "total_duration_millis": total_duration_millis,
                    })
                },
            )
            .collect();
        traces.sort_by(|a, b| {
            b.get("started_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(
                    a.get("started_at")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
        });
        traces.truncate(limit);
        Ok(json!({
            "count": traces.len(),
            "traces": traces,
            "source_path": ".cyclaw/traces.jsonl"
        }))
    }

    /// 还原一条完整链路：span 树 + 大模型推理明细 + 关联执行事件。
    fn get_trace(&self, arguments: Value) -> Result<Value> {
        let trace_id = required_string(&arguments, "trace_id")?;
        let spans: Vec<Value> = read_trace_spans(&self.project_root)?
            .into_iter()
            .filter(|span| span.trace_id == trace_id)
            .map(|span| json!(span))
            .collect();
        let model_calls: Vec<ModelCallRecord> = read_model_calls(&self.project_root)?
            .into_iter()
            .filter(|call| call.trace_id.as_deref() == Some(trace_id))
            .collect();
        let executions: Vec<Value> = read_execution_events(&self.project_root)?
            .into_iter()
            .filter(|event| event.trace_id.as_deref() == Some(trace_id))
            .map(|event| json!(event))
            .collect();
        Ok(json!({
            "trace_id": trace_id,
            "span_count": spans.len(),
            "spans": spans,
            "model_calls": model_calls,
            "execution_events": executions,
            "source_path": ".cyclaw/traces.jsonl"
        }))
    }

    fn list_model_calls(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let provider = arguments.get("provider").and_then(Value::as_str);
        let mut calls: Vec<ModelCallRecord> = read_model_calls(&self.project_root)?
            .into_iter()
            .filter(|call| provider.map(|name| call.provider == name).unwrap_or(true))
            .collect();
        calls.reverse();
        calls.truncate(limit);
        Ok(json!({
            "count": calls.len(),
            "calls": calls,
            "source_path": ".cyclaw/model-calls.jsonl"
        }))
    }

    /// 读取失败任务重试队列，可按状态过滤。
    fn list_retries(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let status = arguments.get("status").and_then(Value::as_str);
        let mut tasks: Vec<cyclaw_events::RetryTask> = read_retry_queue(&self.project_root)?
            .into_iter()
            .filter(|task| {
                status
                    .map(|name| {
                        matches!(
                            (name, &task.status),
                            ("pending", RetryStatus::Pending)
                                | ("in_progress", RetryStatus::InProgress)
                                | ("completed", RetryStatus::Completed)
                                | ("exhausted", RetryStatus::Exhausted)
                                | ("abandoned", RetryStatus::Abandoned)
                        )
                    })
                    .unwrap_or(true)
            })
            .collect();
        tasks.reverse();
        tasks.truncate(limit);
        Ok(json!({
            "count": tasks.len(),
            "tasks": tasks,
            "source_path": ".cyclaw/retry-queue.json"
        }))
    }

    /// 立即执行所有到期重试。
    fn run_retries(&self, arguments: Value) -> Result<Value> {
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let result = run_due_retries(RetryRunOptions {
            project_root: self.project_root.clone(),
            limit,
        })?;
        Ok(json!({
            "executed": result.executed,
            "completed": result.completed,
            "scheduled": result.scheduled,
            "exhausted": result.exhausted,
            "outcomes": result.outcomes,
        }))
    }

    /// 人工放弃一条重试任务。
    fn abandon_retry(&self, arguments: Value) -> Result<Value> {
        let retry_id = required_string(&arguments, "retry_id")?;
        let outcome = abandon_retry_task(&self.project_root, retry_id)?;
        Ok(json!({ "outcome": outcome }))
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

    fn record_execution_event(&self, arguments: Value) -> Result<Value> {
        let kind = match required_string(&arguments, "kind")? {
            "command" => ExecutionEventKind::Command,
            "build" => ExecutionEventKind::Build,
            "test" => ExecutionEventKind::Test,
            "patch" => ExecutionEventKind::Patch,
            value => anyhow::bail!("未知执行事件类型: {}", value),
        };
        let command_summary = required_string(&arguments, "command_summary")?;
        let exit_code = arguments
            .get("exit_code")
            .and_then(Value::as_i64)
            .map(|value| value as i32);
        let timed_out = arguments
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let error_summary = arguments
            .get("error_summary")
            .and_then(Value::as_str)
            .map(|value| value.chars().take(2000).collect());
        let related_files = optional_string_array(&arguments, "related_files")?;
        let session_id = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let trace_id = arguments
            .get("trace_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let duration_millis = arguments
            .get("duration_millis")
            .and_then(Value::as_u64)
            .map(|value| value as u128);
        let event = new_execution_event_with_context(
            "mcp",
            kind,
            command_summary,
            exit_code,
            timed_out,
            related_files,
            error_summary,
            session_id,
            trace_id,
            None,
            duration_millis,
        );
        let result = record_execution_event(self.project_root.clone(), event)?;
        Ok(json!({"duplicate":result.duplicate,"candidate":result.candidate}))
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
        let (docs, model) = match strategy {
            "observe" => (false, false),
            "review" => (true, false),
            "smart" => (true, true),
            _ => anyhow::bail!("未知运行策略: {}", strategy),
        };
        set_permission(&self.project_root, "allow_docs_apply", docs)?;
        set_permission(&self.project_root, "allow_model_call", model)?;
        set_permission(&self.project_root, "allow_network", model)?;
        let policy = load_or_default(&self.project_root)?;
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
        options.session_id = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Some(phase) = arguments.get("phase").and_then(Value::as_str) {
            options.phase = parse_task_phase(phase)?;
        }
        options.related_files = optional_string_array(&arguments, "related_files")?;
        options.context_budget_tokens = arguments
            .get("context_budget_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(2_000)
            .clamp(256, 16_000) as usize;
        let task = begin_project_task(options)?;
        let context = core_get_task_context_for_session(
            &self.project_root,
            Some(&task.id),
            None,
            Some(task.context_budget_tokens),
            20,
            task.session_id.as_deref(),
        )?;
        Ok(json!({"task":task,"context":context}))
    }

    fn get_active_task(&self, arguments: Value) -> Result<Value> {
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        Ok(json!({"task":core_get_active_task_for_session(&self.project_root, session_id)?}))
    }

    fn get_task_context(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let session_id = arguments.get("session_id").and_then(Value::as_str);
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
        Ok(json!(core_get_task_context_for_session(
            &self.project_root,
            task_id,
            query,
            budget,
            limit,
            session_id,
        )?))
    }

    fn record_decision(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        let statement = required_string(&arguments, "statement")?.to_string();
        let rationale = required_string(&arguments, "rationale")?.to_string();
        let evidence = optional_string_array(&arguments, "evidence")?;
        let confidence = arguments
            .get("confidence")
            .and_then(Value::as_u64)
            .unwrap_or(90)
            .min(100) as u8;
        let (task, fact) = record_task_decision_for_session(
            &self.project_root,
            task_id,
            statement,
            rationale,
            evidence,
            confidence,
            session_id,
        )?;
        Ok(json!({"task":task,"fact":fact}))
    }

    fn record_failed_approach(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        let approach = required_string(&arguments, "approach")?.to_string();
        let reason = required_string(&arguments, "reason")?.to_string();
        let evidence = optional_string_array(&arguments, "evidence")?;
        let (task, fact) = record_task_failed_approach_for_session(
            &self.project_root,
            task_id,
            approach,
            reason,
            evidence,
            session_id,
        )?;
        Ok(json!({"task":task,"fact":fact}))
    }

    fn checkpoint_task(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        let summary = required_string(&arguments, "summary")?.to_string();
        let related_files = optional_string_array(&arguments, "related_files")?;
        Ok(json!({"task":checkpoint_project_task_for_session(
            &self.project_root,
            task_id,
            summary,
            related_files,
            session_id,
        )?}))
    }

    fn set_task_phase(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments.get("task_id").and_then(Value::as_str);
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        let phase = parse_task_phase(required_string(&arguments, "phase")?)?;
        Ok(
            json!({"task":set_project_task_phase_for_session(&self.project_root, task_id, phase, session_id)?}),
        )
    }

    fn reconcile_project_knowledge(&self, arguments: Value) -> Result<Value> {
        let task_id = arguments
            .get("task_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                core_get_active_task_for_session(
                    &self.project_root,
                    arguments.get("session_id").and_then(Value::as_str),
                )
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
        let session_id = arguments.get("session_id").and_then(Value::as_str);
        let summary = required_string(&arguments, "summary")?.to_string();
        let reconcile = arguments
            .get("reconcile")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(json!(close_project_task_for_session(
            &self.project_root,
            task_id,
            summary,
            reconcile,
            session_id,
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

fn parse_task_phase(value: &str) -> Result<TaskPhase> {
    match value {
        "investigate" => Ok(TaskPhase::Investigate),
        "design" => Ok(TaskPhase::Design),
        "implement" => Ok(TaskPhase::Implement),
        "verify" => Ok(TaskPhase::Verify),
        "handoff" => Ok(TaskPhase::Handoff),
        _ => anyhow::bail!("未知任务阶段: {}", value),
    }
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
            | "record_execution_event"
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
            | "set_task_phase"
            | "reconcile_project_knowledge"
            | "close_task"
            | "run_retries"
            | "abandon_retry"
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
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
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
            fallback_provider: None,
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
