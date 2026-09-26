use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use cyclaw_agent::{
    AgentRunOptions, RetryRunOptions, abandon_retry_task, cleanup_agent_runs, list_agent_runs,
    run_agent_once, run_due_retries,
};
use cyclaw_core::{
    BeginTaskOptions, DiffOptions, DraftOptions, EvidenceHashScope, FactEvidence, FactInput,
    FactOperation, FactPatch, FactPatchQuery, FactPatchRequest, FactPatchStatus, FactType,
    InboxGenerateOptions, InitOptions, ProjectFact, ScanOptions, SearchOptions,
    analyze_project_diff, apply_document_patch, apply_fact_patch, begin_task, checkpoint_task,
    close_task, current_change_snapshot, diagnose_fact_patch_transactions,
    generate_document_drafts, generate_inbox, get_active_task, get_latest_reconciliation,
    get_task_context, index_project, init_project, list_document_patches, list_inbox,
    list_project_facts, list_tasks, maintain_project_knowledge, observe_execution_artifacts,
    observe_project_once, observer_health, preview_fact_patch, project_fact_from_input,
    project_status, query_evidence_verifications, query_fact_patches, reconcile_project_knowledge,
    record_execution_event, record_observer_error, record_observer_metrics, record_task_decision,
    record_task_failed_approach, revert_document_patch, revert_fact_patch, scan_project,
    search_project, update_inbox_status, verify_fact_evidence, watch_project_once,
};
use cyclaw_docs::KnowledgeOperation;
use cyclaw_events::{
    ExecutionEventKind, ModelCallRecord, RetryStatus, TraceSpan, TraceSpanKind, TraceSpanStatus,
    append_trace_span, new_execution_event_with_context, new_span_id, new_trace_id, new_trace_span,
    now_rfc3339, read_execution_events, read_model_calls, read_retry_queue, read_trace_spans,
};
use cyclaw_knowledge::{KnowledgeImportance, KnowledgeStatus};
use cyclaw_model::{
    AddProviderOptions, TestProviderOptions, add_provider, list_providers, test_provider,
    use_provider,
};
use cyclaw_policy::{
    PermissionLevel, acquire_lock, check_write_path, load_or_default, set_permission,
};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tempfile::NamedTempFile;

#[derive(Parser)]
#[command(name = "cyclaw")]
#[command(version)]
#[command(about = "AI Coding 时代的项目知识维护层")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 初始化当前项目的 cyClaw 知识目录
    Init {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 扫描项目并刷新项目画像
    Scan {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看 cyClaw 当前项目状态
    Status {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 分析 Git 变更并生成知识资产影响报告
    Diff {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 执行命令并自动记录执行事件；命令参数放在 -- 后
    Exec {
        /// 执行类型
        #[arg(long, default_value = "command")]
        kind: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 当前 Coding Agent 会话 ID
        #[arg(long)]
        session_id: Option<String>,
        /// 当前调用链追踪 ID
        #[arg(long)]
        trace_id: Option<String>,
        /// 关联代码或配置文件，可重复传入
        #[arg(long = "related-file")]
        related_files: Vec<String>,
        /// 超时时间，单位秒；0 表示不设超时
        #[arg(long, default_value_t = 0)]
        timeout_seconds: u64,
        /// 要执行的命令，使用 `--` 与 cyClaw 参数分隔
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// 常驻监听 Git 变更，自动生成变更雷达报告
    Watch {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 兼容旧版本的事件合并秒数，建议改用 --debounce-ms
        #[arg(short, long)]
        interval: Option<u64>,
        /// 文件事件合并窗口，单位毫秒
        #[arg(long, default_value_t = 600)]
        debounce_ms: u64,
        /// 只检查一次，用于调试和测试
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    /// 运行独立项目观察器；不依赖 Coding Agent、MCP 调用或 IDE 生命周期
    Observer {
        #[command(subcommand)]
        command: ObserverCommand,
    },
    /// 管理知识收件箱
    Inbox {
        #[command(subcommand)]
        command: InboxCommand,
    },
    /// 生成和应用文档草稿
    Draft {
        #[command(subcommand)]
        command: DraftCommand,
    },
    /// 管理结构化 Fact Ledger 草稿
    Fact {
        #[command(subcommand)]
        command: FactCommand,
    },
    /// 构建本地知识索引
    Index {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 搜索本地项目知识
    Search {
        /// 搜索关键词
        query: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 返回数量上限
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// 启动 MCP stdio 服务，供 AI 编码工具读取项目知识
    Mcp {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 管理用户自带的大模型 Provider
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// 运行 cyClaw 项目知识 Agent
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// 管理项目任务记忆和上下文
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// 查看和验证 cyClaw 权限策略
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// 查看 cyClaw 事件日志
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// 查询本地链路追踪数据
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    /// 管理失败任务的重试队列
    Retry {
        #[command(subcommand)]
        command: RetryCommand,
    },
    /// 管理 cyClaw 生命周期 Hooks
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
}

#[derive(Subcommand)]
enum InboxCommand {
    /// 从最近一次变更分析生成候选知识
    Generate {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 指定 change-analysis.json 路径，默认使用最近一次分析
        #[arg(short, long)]
        analysis: Option<PathBuf>,
    },
    /// 查看知识收件箱
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 是否只显示 pending 状态
        #[arg(long, default_value_t = false)]
        pending: bool,
    },
    /// 接受候选知识
    Accept {
        /// 候选知识 ID
        id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 忽略候选知识
    Ignore {
        /// 候选知识 ID
        id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum DraftCommand {
    /// 从候选知识生成文档草稿
    Generate {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 指定候选知识 ID
        #[arg(short, long)]
        candidate: Option<String>,
        /// 同时处理 pending 状态候选知识
        #[arg(long, default_value_t = false)]
        include_pending: bool,
        /// 知识操作：create/update/merge/supersede/delete
        #[arg(long)]
        operation: Option<String>,
        /// 目标 Markdown 章节标题，或 candidate:<候选ID>
        #[arg(long)]
        selector: Option<String>,
        /// merge 操作的来源章节标题，可重复传入
        #[arg(long = "source-selector")]
        source_selectors: Vec<String>,
        /// 替换内容文件，省略时使用候选知识生成的标准章节
        #[arg(long)]
        replacement_file: Option<PathBuf>,
        /// delete 操作删除整份目标文档，而不是单个章节
        #[arg(long, default_value_t = false)]
        delete_document: bool,
    },
    /// 查看文档草稿
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 应用指定文档草稿
    Apply {
        /// 文档草稿 ID
        id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 撤销已应用的文档草稿
    Revert {
        /// 文档草稿 ID
        id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Args)]
struct EvidenceOptions {
    /// 证据路径；结构化定位参数存在时只允许一个路径
    #[arg(long = "evidence")]
    paths: Vec<String>,
    /// 证据对应的符号名称
    #[arg(long = "evidence-symbol")]
    symbol: Option<String>,
    /// 证据起始行，从 1 开始
    #[arg(long = "evidence-line-start")]
    line_start: Option<u32>,
    /// 证据结束行，默认等于起始行
    #[arg(long = "evidence-line-end")]
    line_end: Option<u32>,
    /// 哈希范围：file、line_range、symbol
    #[arg(long = "evidence-hash-scope")]
    hash_scope: Option<EvidenceHashScope>,
    /// 证据类型，例如 file、source、config
    #[arg(long = "evidence-type", default_value = "file")]
    evidence_type: String,
}

impl EvidenceOptions {
    fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.symbol.is_none()
            && self.line_start.is_none()
            && self.line_end.is_none()
            && self.hash_scope.is_none()
    }

    fn into_parts(self) -> Result<(Vec<String>, Vec<FactEvidence>)> {
        let structured = self.symbol.is_some()
            || self.line_start.is_some()
            || self.line_end.is_some()
            || self.hash_scope.is_some()
            || self.evidence_type != "file";
        if !structured {
            return Ok((self.paths, Vec::new()));
        }
        if self.paths.len() != 1 {
            anyhow::bail!("结构化证据定位参数要求且只允许一个 --evidence 路径");
        }
        if self.line_end.is_some() && self.line_start.is_none() {
            anyhow::bail!("--evidence-line-end 必须与 --evidence-line-start 一起使用");
        }
        let hash_scope = self.hash_scope.or_else(|| {
            if self.symbol.is_some() {
                Some(EvidenceHashScope::Symbol)
            } else if self.line_start.is_some() {
                Some(EvidenceHashScope::LineRange)
            } else {
                Some(EvidenceHashScope::File)
            }
        });
        if hash_scope == Some(EvidenceHashScope::Symbol) && self.symbol.is_none() {
            anyhow::bail!("symbol 哈希范围必须提供 --evidence-symbol");
        }
        if hash_scope == Some(EvidenceHashScope::LineRange) && self.line_start.is_none() {
            anyhow::bail!("line_range 哈希范围必须提供 --evidence-line-start");
        }
        let path = self.paths[0].clone();
        Ok((
            vec![path.clone()],
            vec![FactEvidence {
                path,
                symbol: self.symbol,
                line_start: self.line_start,
                line_end: self.line_end,
                content_hash: None,
                hash_scope,
                git_head: None,
                captured_at: String::new(),
                verified_at: String::new(),
                evidence_type: self.evidence_type,
            }],
        ))
    }
}

#[derive(Subcommand)]
enum FactCommand {
    /// 新增事实；默认只生成草稿
    Create {
        statement: String,
        #[arg(long, default_value = "unknown")]
        fact_type: String,
        #[command(flatten)]
        evidence: EvidenceOptions,
        #[arg(long, default_value_t = 90)]
        confidence: u8,
        #[arg(long, default_value_t = false)]
        apply: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 更新 Active Fact 并保留原 ID
    Update {
        id: String,
        #[arg(long)]
        statement: Option<String>,
        #[arg(long)]
        fact_type: Option<String>,
        #[command(flatten)]
        evidence: EvidenceOptions,
        #[arg(long)]
        confidence: Option<u8>,
        #[arg(long, default_value_t = false)]
        apply: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 合并重复事实并保留主 Fact
    Merge {
        target: String,
        #[arg(long = "source", required = true)]
        sources: Vec<String>,
        #[arg(long)]
        statement: Option<String>,
        #[arg(long, default_value_t = false)]
        apply: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 用新事实取代旧 Active Fact
    Supersede {
        target: String,
        statement: String,
        #[arg(long, default_value = "unknown")]
        fact_type: String,
        #[command(flatten)]
        evidence: EvidenceOptions,
        #[arg(long, default_value_t = 90)]
        confidence: u8,
        #[arg(long, default_value_t = false)]
        apply: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 将 Active Fact 标记为 Deleted
    Delete {
        id: String,
        #[arg(long, default_value_t = false)]
        apply: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 校验证据路径、符号、行范围和内容哈希
    Verify {
        id: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 预览 create/update/merge/supersede/delete 事实操作
    Preview {
        #[arg(long)]
        operation: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long = "source")]
        sources: Vec<String>,
        /// 完整 ProjectFact JSON 文件；create/update/supersede 必填，merge 可选
        #[arg(long)]
        fact_file: Option<PathBuf>,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看事实治理草稿
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        operation: Option<String>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看独立证据验证账本
    Verifications {
        #[arg(long)]
        fact_id: Option<String>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 只读诊断待恢复的事实事务
    Transactions {
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 应用事实治理草稿
    Apply {
        id: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 撤销已应用的事实治理草稿
    Revert {
        id: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModelCommand {
    /// 添加 OpenAI-compatible 模型 Provider
    Add {
        /// Provider 名称，例如 deepseek
        name: String,
        /// API Base URL，例如 https://api.deepseek.com
        #[arg(long)]
        base_url: String,
        /// 模型名称，例如 deepseek-v4-flash
        #[arg(long)]
        model: String,
        /// 保存 API Key 的环境变量名
        #[arg(long, default_value = "DEEPSEEK_API_KEY")]
        api_key_env: String,
        /// 显式开启模型思考模式，默认关闭
        #[arg(long, default_value_t = false)]
        thinking: bool,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 将该 Provider 设置为 active
        #[arg(long, default_value_t = true)]
        active: bool,
        /// 请求失败时自动降级使用的备用 Provider 名称
        #[arg(long)]
        fallback: Option<String>,
    },
    /// 查看已配置模型 Provider
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 切换当前项目的活动模型 Provider
    Use {
        /// Provider 名称
        name: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 测试指定模型 Provider 连通性
    Test {
        /// Provider 名称
        name: String,
        /// 测试提示词
        #[arg(long, default_value = "请用一句话说明 cyClaw 已经成功连接模型。")]
        prompt: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看大模型推理调用明细（推理路径）
    Calls {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 返回数量上限
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// 按 Provider 名称过滤
        #[arg(long)]
        provider: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// 运行一次 Agent 工作流
    Run {
        /// 只运行一次
        #[arg(long, default_value_t = true)]
        once: bool,
        /// 指定模型 Provider，默认使用 active provider
        #[arg(long)]
        provider: Option<String>,
        /// 不调用模型，只运行本地规则和状态收集
        #[arg(long, default_value_t = false)]
        no_model: bool,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 管理 Agent 运行记录
    Runs {
        #[command(subcommand)]
        command: AgentRunsCommand,
    },
}

#[derive(Subcommand)]
enum ObserverCommand {
    /// 前台运行常驻观察器；首次启动会自动补偿扫描已有变更
    Run {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 文件事件合并窗口，单位毫秒
        #[arg(long, default_value_t = 600)]
        debounce_ms: u64,
        /// 周期执行事实证据验证和知识对账的间隔，单位秒
        #[arg(long, default_value_t = 300)]
        maintenance_interval_seconds: u64,
        /// 只执行启动补偿，不进入常驻循环
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    /// 输出 Observer 持久化状态
    Status {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 输出机器可读 JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// 安装当前用户的开机登录观察任务（Windows）并立即启动
    Install {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 卸载当前项目对应的观察任务（Windows）
    Uninstall {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AgentRunsCommand {
    /// 查看最近 Agent 运行记录
    List {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// 清理旧 Agent 运行记录
    Clean {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 20)]
        keep: usize,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    /// 开始任务并生成首个上下文包
    Begin {
        title: String,
        #[arg(long)]
        objective: String,
        #[arg(long = "related-file")]
        related_files: Vec<String>,
        #[arg(long, default_value_t = 2_000)]
        context_budget_tokens: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看当前活动任务
    Active {
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 编译当前或指定任务的上下文包
    Context {
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        budget_tokens: Option<usize>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 记录关键决策
    Decision {
        statement: String,
        #[arg(long)]
        rationale: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        #[arg(long, default_value_t = 90)]
        confidence: u8,
        #[arg(long)]
        task: Option<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 记录失败方案
    Failure {
        approach: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "evidence")]
        evidence: Vec<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 保存长任务检查点
    Checkpoint {
        summary: String,
        #[arg(long = "related-file")]
        related_files: Vec<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 执行事实重复、冲突和失效对账
    Reconcile {
        #[arg(long)]
        task: Option<String>,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 关闭任务并默认执行知识对账
    Close {
        summary: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        reconcile: bool,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看最近任务
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看结构化项目事实
    Facts {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 查看最近知识对账报告
    LatestReconciliation {
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// 查看当前权限策略
    Show {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 检查目标路径在指定权限层级下是否可写
    Check {
        /// 目标路径，例如 docs/api.md
        target: String,
        /// 权限层级：read_only/local_knowledge_write/docs_write/model_call/automation/shell
        #[arg(long, default_value = "local_knowledge_write")]
        level: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 设置权限开关
    Set {
        /// 权限键：allow_model_call/allow_network/allow_docs_apply
        key: String,
        /// 是否启用
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        enabled: bool,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 启用模型调用及联网权限
    EnableModel {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 启用文档草稿应用权限
    EnableDocsApply {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EventsCommand {
    /// 查看最近事件
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 返回数量上限
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum TraceCommand {
    /// 查看最近的链路追踪
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 返回数量上限
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// 按 trace_id 还原完整调用链（含大模型推理路径）
    Show {
        /// 链路 ID；Agent 运行记录的 run_id 即其 trace_id
        trace_id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum RetryCommand {
    /// 查看重试队列
    List {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 返回数量上限
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// 按状态过滤：pending/in_progress/completed/exhausted/abandoned
        #[arg(long)]
        status: Option<String>,
    },
    /// 立即执行所有到期重试；恢复失败上下文后重新运行模型审查
    Run {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 单次执行上限
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// 人工放弃一条重试任务，降级为人工处理
    Abandon {
        /// 重试任务 ID
        retry_id: String,
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum HooksCommand {
    /// 运行一个 Hook 事件
    Run {
        /// 事件：session-start/session-stop/pre-commit
        event: String,
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// 高置信度知识未处理时返回失败
        #[arg(long, default_value_t = false)]
        strict: bool,
    },
    /// 安装 Git pre-commit Hook
    InstallGit {
        #[arg(short, long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// 查看 Git Hook 安装状态
    Status {
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 卸载 cyClaw Git Hook
    UninstallGit {
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            let result = init_project(InitOptions::new(resolve_path(path)?))?;
            println!("cyClaw 初始化完成");
            println!("项目根目录: {}", result.project_root.display());
            println!("配置文件: {}", result.config_path.display());
            println!("项目画像: {}", result.profile_path.display());
            println!("项目说明: {}", result.project_doc_path.display());
        }
        Commands::Scan { path } => {
            let result = scan_project(ScanOptions::new(resolve_path(path)?))?;
            println!("cyClaw 扫描完成");
            println!("项目根目录: {}", result.project_root.display());
            println!("项目画像: {}", result.profile_path.display());
            println!("识别语言: {}", result.profile.languages.join(", "));
            println!("识别框架: {}", result.profile.frameworks.join(", "));
            println!("依赖文件: {}", result.profile.dependency_files.len());
            println!("文档入口: {}", result.profile.document_paths.len());
            println!("配置文件: {}", result.profile.config_files.len());
        }
        Commands::Status { path } => {
            let status = project_status(resolve_path(path)?)?;
            println!("cyClaw 项目状态");
            println!("项目根目录: {}", status.project_root.display());
            println!("已初始化: {}", render_bool(status.initialized));
            println!("配置文件: {}", render_bool(status.config_exists));
            println!("项目画像: {}", render_bool(status.project_profile_exists));
            println!("项目说明: {}", render_bool(status.project_doc_exists));
            println!(
                "最近变更分析: {}",
                status
                    .latest_run
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "无".to_string())
            );
            println!("Git 未提交变更: {}", render_bool(status.git_has_changes));
            println!("知识收件箱: {}", render_bool(status.inbox_exists));
            println!("候选知识总数: {}", status.inbox_total);
            println!("待处理候选知识: {}", status.inbox_pending);
            println!("文档草稿总数: {}", status.draft_total);
            println!("待应用文档草稿: {}", status.draft_pending);
            println!("事实草稿总数: {}", status.fact_patch_total);
            println!("待应用事实草稿: {}", status.fact_patch_pending);
            println!("可撤销事实草稿: {}", status.fact_patch_revertible);
            println!("本地索引: {}", render_bool(status.index_exists));
            println!();
            println!("建议下一步:");
            for step in &status.suggested_next_steps {
                println!("- {}", step);
            }
        }
        Commands::Diff { path } => {
            let result = analyze_project_diff(DiffOptions::new(resolve_path(path)?))?;
            println!("cyClaw 变更雷达分析完成");
            println!("项目根目录: {}", result.project_root.display());
            println!("运行 ID: {}", result.run_id);
            println!("分析文件: {}", result.analysis_path.display());
            println!("变更文件: {}", result.analysis.summary.total_files);
            println!("API 变化: {}", result.analysis.summary.api_changes);
            println!("Schema 变化: {}", result.analysis.summary.schema_changes);
            println!("依赖变化: {}", result.analysis.summary.dependency_changes);
            println!("配置变化: {}", result.analysis.summary.config_changes);
            println!(
                "环境变量变化: {}",
                result.analysis.summary.environment_changes
            );
            println!("受影响知识资产: {}", result.analysis.impacted_assets.len());

            for asset in &result.analysis.impacted_assets {
                println!("- {}：{}", asset.asset, asset.reason);
            }
        }
        Commands::Exec {
            kind,
            path,
            session_id,
            trace_id,
            related_files,
            timeout_seconds,
            command,
        } => {
            let project_root = resolve_path(path)?;
            let event_kind = match kind.as_str() {
                "command" => ExecutionEventKind::Command,
                "build" => ExecutionEventKind::Build,
                "test" => ExecutionEventKind::Test,
                "patch" => ExecutionEventKind::Patch,
                value => anyhow::bail!("不支持的执行类型: {}", value),
            };
            let executable = command.first().context("缺少要执行的命令")?;
            let started = std::time::Instant::now();
            // 链路追踪：外部传入 trace_id 时沿用，否则为本次执行生成独立链路。
            let resolved_trace_id = trace_id.clone().unwrap_or_else(new_trace_id);
            let span_id = new_span_id();
            let span_started_at = now_rfc3339();
            let command_summary = command.join(" ");
            // 使用临时文件承接输出，避免大输出填满管道导致子进程死锁。
            let stdout_file = NamedTempFile::new()?;
            let stderr_file = NamedTempFile::new()?;
            let mut child = Command::new(executable)
                .args(&command[1..])
                .current_dir(&project_root)
                .stdout(std::process::Stdio::from(stdout_file.reopen()?))
                .stderr(std::process::Stdio::from(stderr_file.reopen()?))
                .spawn()?;
            let timeout = (timeout_seconds > 0).then(|| Duration::from_secs(timeout_seconds));
            loop {
                if let Some(status) = child.try_wait()? {
                    let stderr = fs::read_to_string(stderr_file.path()).unwrap_or_default();
                    let stdout = fs::read_to_string(stdout_file.path()).unwrap_or_default();
                    let error_summary =
                        (!stderr.trim().is_empty()).then(|| truncate_output(&stderr));
                    let event = new_execution_event_with_context(
                        "cyclaw-cli",
                        event_kind,
                        command_summary.clone(),
                        status.code(),
                        false,
                        related_files.clone(),
                        error_summary,
                        session_id,
                        trace_id,
                        git_head(&project_root),
                        Some(started.elapsed().as_millis()),
                    );
                    let result = record_execution_event(project_root.clone(), event)?;
                    let _ = append_trace_span(
                        &project_root,
                        &new_trace_span(
                            resolved_trace_id,
                            span_id,
                            None,
                            "exec",
                            TraceSpanKind::Exec,
                            "cyclaw-cli",
                            if status.success() {
                                TraceSpanStatus::Ok
                            } else {
                                TraceSpanStatus::Error
                            },
                            span_started_at,
                            now_rfc3339(),
                            serde_json::json!({
                                "kind": kind,
                                "command": command_summary,
                                "exit_code": status.code(),
                                "timed_out": false,
                                "related_files": related_files,
                            }),
                        ),
                    );
                    print!("{}", stdout);
                    eprint!("{}", stderr);
                    eprintln!("cyClaw 执行事件已记录: duplicate={}", result.duplicate);
                    std::process::exit(status.code().unwrap_or(1));
                }
                if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                    let _ = child.kill();
                    let status = child.wait()?;
                    let stderr = fs::read_to_string(stderr_file.path()).unwrap_or_default();
                    let event = new_execution_event_with_context(
                        "cyclaw-cli",
                        event_kind,
                        command_summary.clone(),
                        status.code(),
                        true,
                        related_files.clone(),
                        Some(if stderr.trim().is_empty() {
                            format!("命令执行超过 {} 秒", timeout_seconds)
                        } else {
                            format!(
                                "命令执行超过 {} 秒: {}",
                                timeout_seconds,
                                truncate_output(&stderr)
                            )
                        }),
                        session_id,
                        trace_id,
                        git_head(&project_root),
                        Some(started.elapsed().as_millis()),
                    );
                    let result = record_execution_event(project_root.clone(), event)?;
                    let _ = append_trace_span(
                        &project_root,
                        &new_trace_span(
                            resolved_trace_id,
                            span_id,
                            None,
                            "exec",
                            TraceSpanKind::Exec,
                            "cyclaw-cli",
                            TraceSpanStatus::Timeout,
                            span_started_at,
                            now_rfc3339(),
                            serde_json::json!({
                                "kind": kind,
                                "command": command_summary,
                                "exit_code": status.code(),
                                "timed_out": true,
                                "related_files": related_files,
                            }),
                        ),
                    );
                    eprintln!(
                        "cyClaw 执行超时，事件已记录: duplicate={}",
                        result.duplicate
                    );
                    std::process::exit(124);
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        Commands::Observer { command } => match command {
            ObserverCommand::Run {
                path,
                debounce_ms,
                maintenance_interval_seconds,
                once,
            } => {
                let project_root = resolve_path(path)?;
                let _observer_lock =
                    acquire_lock(&project_root, "observer", Duration::from_secs(1))?;
                println!("cyClaw 独立观察器已启动");
                println!("项目根目录: {}", project_root.display());
                println!("事件源: 文件系统、Git 快照、测试与构建报告、事实证据");
                let rescan_required = Arc::new(AtomicBool::new(false));
                let (sender, receiver) = mpsc::sync_channel(256);
                let dropped_flag = Arc::clone(&rescan_required);
                let _watcher = if once {
                    None
                } else {
                    let mut watcher = notify::recommended_watcher(move |event| {
                        if sender.try_send(event).is_err() {
                            dropped_flag.store(true, Ordering::Release);
                        }
                    })?;
                    watcher.watch(&project_root, RecursiveMode::Recursive)?;
                    Some(watcher)
                };
                let initial = match observe_project_once(&project_root) {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = record_observer_error(&project_root, &error);
                        return Err(error);
                    }
                };
                print_observer_tick(&initial);
                if once {
                    return Ok(());
                }

                let debounce = Duration::from_millis(debounce_ms.max(100));
                let maintenance = Duration::from_secs(maintenance_interval_seconds.max(30));
                loop {
                    match receiver.recv_timeout(maintenance) {
                        Ok(Ok(event)) => {
                            let _ = record_observer_metrics(&project_root, 1, 0, 0, 0, 0);
                            if !is_relevant_watch_event(&project_root, &event) {
                                continue;
                            }
                            let mut paths = event.paths;
                            thread::sleep(debounce);
                            let mut coalesced = 0;
                            while let Ok(Ok(next)) = receiver.try_recv() {
                                paths.extend(next.paths);
                                coalesced += 1;
                            }
                            if coalesced > 0 {
                                let _ =
                                    record_observer_metrics(&project_root, 0, coalesced, 0, 0, 0);
                            }
                            let artifact_events =
                                match observe_execution_artifacts(&project_root, &paths) {
                                    Ok(value) => value,
                                    Err(error) => {
                                        let _ = record_observer_error(&project_root, &error);
                                        return Err(error);
                                    }
                                };
                            let tick = match observe_project_once(&project_root) {
                                Ok(value) => value,
                                Err(error) => {
                                    let _ = record_observer_error(&project_root, &error);
                                    return Err(error);
                                }
                            };
                            if tick.watch.changed || artifact_events > 0 {
                                print_observer_tick(&tick);
                                if artifact_events > 0 {
                                    println!("自动采集失败报告: {}", artifact_events);
                                }
                            }
                        }
                        Ok(Err(error)) => eprintln!("文件观察事件异常: {}", error),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if rescan_required.swap(false, Ordering::AcqRel) {
                                let _ = record_observer_metrics(&project_root, 0, 0, 1, 1, 1);
                                if let Err(error) = observe_project_once(&project_root) {
                                    let _ = record_observer_error(&project_root, &error);
                                    return Err(error);
                                }
                            }
                            let (verified, report) = match maintain_project_knowledge(&project_root)
                            {
                                Ok(value) => value,
                                Err(error) => {
                                    let _ = record_observer_error(&project_root, &error);
                                    return Err(error);
                                }
                            };
                            println!(
                                "Observer 周期维护完成: 验证事实 {} 条，重复 {}，冲突 {}，失效 {}，漂移 {}",
                                verified,
                                report.duplicate_count,
                                report.conflict_count,
                                report.stale_count,
                                report.drift_count
                            );
                            // 空闲期自动恢复：执行所有到期的失败重试，无需人工值守。
                            match run_due_retries(RetryRunOptions {
                                project_root: project_root.clone(),
                                limit: 5,
                            }) {
                                Ok(result) if result.executed > 0 => {
                                    println!(
                                        "Observer 重试恢复: 执行 {}，完成 {}，继续退避 {}，耗尽降级 {}",
                                        result.executed,
                                        result.completed,
                                        result.scheduled,
                                        result.exhausted
                                    );
                                    for outcome in &result.outcomes {
                                        println!(
                                            "- [{:?}] {} {}",
                                            outcome.status, outcome.retry_id, outcome.detail
                                        );
                                    }
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    // 重试恢复失败不影响观察器主循环，下一周期再试。
                                    let _ = record_observer_error(&project_root, &error);
                                }
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            anyhow::bail!("文件观察通道意外关闭");
                        }
                    }
                }
            }
            ObserverCommand::Status { path, json } => {
                let project_root = resolve_path(path)?;
                let state_path = cyclaw_core::observer_state_path(&project_root);
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&observer_health(&project_root)?)?
                    );
                    return Ok(());
                }
                if !state_path.exists() {
                    println!("Observer 尚未在此项目运行");
                } else {
                    println!("{}", fs::read_to_string(state_path)?);
                }
            }
            ObserverCommand::Install { path } => {
                let project_root = resolve_path(path)?;
                install_observer_task(&project_root)?;
            }
            ObserverCommand::Uninstall { path } => {
                let project_root = resolve_path(path)?;
                uninstall_observer_task(&project_root)?;
            }
        },
        Commands::Watch {
            path,
            interval,
            debounce_ms,
            once,
        } => {
            let project_root = resolve_path(path)?;
            println!("cyClaw watch 为兼容入口；生产环境请使用 `cyclaw observer run`");
            println!("项目根目录: {}", project_root.display());
            println!("监听模式: 文件系统事件 + Git 增量快照");

            if once {
                let tick = watch_project_once(&project_root, None)?;
                print_watch_tick(tick);
                return Ok(());
            }

            let debounce = Duration::from_millis(
                interval
                    .map(|seconds| seconds.saturating_mul(1000))
                    .unwrap_or(debounce_ms)
                    .max(100),
            );
            println!("事件合并窗口: {} 毫秒", debounce.as_millis());
            let mut last_snapshot = current_change_snapshot(&project_root)?;
            let (sender, receiver) = mpsc::sync_channel(256);
            let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
                let _ = sender.try_send(event);
            })?;
            watcher.watch(&project_root, RecursiveMode::Recursive)?;

            loop {
                let event = receiver.recv()?;
                let Ok(event) = event else {
                    continue;
                };
                if !is_relevant_watch_event(&project_root, &event) {
                    continue;
                }
                thread::sleep(debounce);
                while receiver.try_recv().is_ok() {}
                let tick = watch_project_once(&project_root, Some(&last_snapshot))?;
                let next_snapshot = current_change_snapshot(&project_root)?;

                if tick.changed {
                    print_watch_tick(tick);
                }

                last_snapshot = next_snapshot;
            }
        }
        Commands::Inbox { command } => match command {
            InboxCommand::Generate { path, analysis } => {
                let project_root = resolve_path(path)?;
                let analysis = match analysis {
                    Some(path) => Some(path.canonicalize()?),
                    None => None,
                };
                let result = generate_inbox(InboxGenerateOptions::new(project_root, analysis))?;
                println!("知识候选生成完成");
                println!("来源分析: {}", result.source_analysis_path.display());
                println!("知识收件箱: {}", result.inbox_path.display());
                println!("本次生成: {}", result.generated.len());
                println!("待处理总数: {}", result.total_pending);

                for candidate in &result.generated {
                    print_candidate_summary(candidate);
                }
            }
            InboxCommand::List { path, pending } => {
                let result = list_inbox(resolve_path(path)?)?;
                println!("知识收件箱: {}", result.inbox_path.display());

                let candidates = result
                    .candidates
                    .iter()
                    .filter(|candidate| !pending || candidate.status == KnowledgeStatus::Pending)
                    .collect::<Vec<_>>();

                println!("候选知识数量: {}", candidates.len());

                for candidate in candidates {
                    print_candidate_summary(candidate);
                }
            }
            InboxCommand::Accept { id, path } => {
                let result =
                    update_inbox_status(resolve_path(path)?, &id, KnowledgeStatus::Accepted)?;
                println!("已接受候选知识: {}", result.candidate.id);
                println!("知识收件箱: {}", result.inbox_path.display());
            }
            InboxCommand::Ignore { id, path } => {
                let result =
                    update_inbox_status(resolve_path(path)?, &id, KnowledgeStatus::Ignored)?;
                println!("已忽略候选知识: {}", result.candidate.id);
                println!("知识收件箱: {}", result.inbox_path.display());
            }
        },
        Commands::Fact { command } => match command {
            FactCommand::Create {
                statement,
                fact_type,
                evidence,
                confidence,
                apply,
                path,
            } => {
                let root = resolve_path(path)?;
                let fact = new_fact(statement, fact_type.parse()?, evidence, confidence)?;
                let patch = preview_fact_patch(
                    &root,
                    FactPatchRequest {
                        operation: FactOperation::Create,
                        target_fact_id: None,
                        source_fact_ids: Vec::new(),
                        fact: Some(fact),
                    },
                )?;
                finish_fact_preview(&root, patch, apply)?;
            }
            FactCommand::Update {
                id,
                statement,
                fact_type,
                evidence,
                confidence,
                apply,
                path,
            } => {
                let root = resolve_path(path)?;
                if statement.is_none()
                    && fact_type.is_none()
                    && evidence.is_empty()
                    && confidence.is_none()
                {
                    anyhow::bail!("fact update 至少需要提供一个修改字段");
                }
                let mut fact = find_project_fact(&root, &id)?;
                if let Some(statement) = statement {
                    fact.statement = statement;
                }
                if let Some(fact_type) = fact_type {
                    fact.fact_type = fact_type.parse()?;
                }
                if !evidence.is_empty() {
                    let (paths, details) = evidence.into_parts()?;
                    fact.evidence = paths;
                    fact.evidence_details = details;
                }
                if let Some(confidence) = confidence {
                    fact.confidence = confidence.min(100);
                }
                let patch = preview_fact_patch(
                    &root,
                    FactPatchRequest {
                        operation: FactOperation::Update,
                        target_fact_id: Some(id),
                        source_fact_ids: Vec::new(),
                        fact: Some(fact),
                    },
                )?;
                finish_fact_preview(&root, patch, apply)?;
            }
            FactCommand::Merge {
                target,
                sources,
                statement,
                apply,
                path,
            } => {
                let root = resolve_path(path)?;
                let fact = statement
                    .map(|statement| {
                        let mut fact = find_project_fact(&root, &target)?;
                        fact.statement = statement;
                        Ok::<ProjectFact, anyhow::Error>(fact)
                    })
                    .transpose()?;
                let patch = preview_fact_patch(
                    &root,
                    FactPatchRequest {
                        operation: FactOperation::Merge,
                        target_fact_id: Some(target),
                        source_fact_ids: sources,
                        fact,
                    },
                )?;
                finish_fact_preview(&root, patch, apply)?;
            }
            FactCommand::Supersede {
                target,
                statement,
                fact_type,
                evidence,
                confidence,
                apply,
                path,
            } => {
                let root = resolve_path(path)?;
                let fact = new_fact(statement, fact_type.parse()?, evidence, confidence)?;
                let patch = preview_fact_patch(
                    &root,
                    FactPatchRequest {
                        operation: FactOperation::Supersede,
                        target_fact_id: Some(target),
                        source_fact_ids: Vec::new(),
                        fact: Some(fact),
                    },
                )?;
                finish_fact_preview(&root, patch, apply)?;
            }
            FactCommand::Delete { id, apply, path } => {
                let root = resolve_path(path)?;
                let patch = preview_fact_patch(
                    &root,
                    FactPatchRequest {
                        operation: FactOperation::Delete,
                        target_fact_id: Some(id),
                        source_fact_ids: Vec::new(),
                        fact: None,
                    },
                )?;
                finish_fact_preview(&root, patch, apply)?;
            }
            FactCommand::Verify { id, path } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&verify_fact_evidence(
                        &resolve_path(path)?,
                        &id
                    )?)?
                );
            }
            FactCommand::Preview {
                operation,
                target,
                sources,
                fact_file,
                path,
            } => {
                let fact = match fact_file {
                    Some(file) => Some(serde_json::from_str::<ProjectFact>(&fs::read_to_string(
                        file,
                    )?)?),
                    None => None,
                };
                let patch = preview_fact_patch(
                    &resolve_path(path)?,
                    FactPatchRequest {
                        operation: operation.parse::<FactOperation>()?,
                        target_fact_id: target,
                        source_fact_ids: sources,
                        fact,
                    },
                )?;
                println!("事实草稿: {}", patch.id);
                println!("操作: {:?}", patch.operation);
                println!("预览指纹: {}", patch.preview_fingerprint);
            }
            FactCommand::List {
                status,
                operation,
                offset,
                limit,
                path,
            } => {
                let page = query_fact_patches(
                    &resolve_path(path)?,
                    FactPatchQuery {
                        status: status
                            .map(|value| value.parse::<FactPatchStatus>())
                            .transpose()?,
                        operation: operation
                            .map(|value| value.parse::<FactOperation>())
                            .transpose()?,
                        offset,
                        limit,
                    },
                )?;
                println!("事实草稿总数: {}", page.total);
                for patch in page.patches {
                    println!("{} {:?} {:?}", patch.id, patch.operation, patch.status);
                }
            }
            FactCommand::Verifications {
                fact_id,
                offset,
                limit,
                path,
            } => {
                let page = query_evidence_verifications(
                    &resolve_path(path)?,
                    fact_id.as_deref(),
                    offset,
                    limit,
                )?;
                println!(
                    "验证记录总数: {}，当前偏移: {}，返回: {}",
                    page.total,
                    page.offset,
                    page.records.len()
                );
                for issue in page.issues {
                    eprintln!("账本诊断: {}:{} {}", issue.path, issue.line, issue.reason);
                }
                for record in page.records {
                    println!(
                        "{} {} 已验证={} 问题={}",
                        record.checked_at,
                        record.fact_id,
                        record.verified_count,
                        record.issue_count
                    );
                }
            }
            FactCommand::Transactions { path } => {
                let diagnostics = diagnose_fact_patch_transactions(&resolve_path(path)?)?;
                println!("待恢复事务: {}", diagnostics.pending_count);
                println!("可恢复: {}", diagnostics.recoverable_count);
                println!("阻塞: {}", diagnostics.blocked_count);
                for transaction in diagnostics.transactions {
                    println!(
                        "{} {:?} {}",
                        transaction.patch_id.as_deref().unwrap_or("未知 Patch"),
                        transaction.status,
                        transaction.reason
                    );
                }
            }
            FactCommand::Apply { id, path } => {
                let patch = apply_fact_patch(&resolve_path(path)?, &id)?;
                println!(
                    "已应用事实草稿: {}；可使用 `cyclaw fact revert {}` 撤销",
                    patch.id, patch.id
                );
            }
            FactCommand::Revert { id, path } => {
                let patch = revert_fact_patch(&resolve_path(path)?, &id)?;
                println!("已撤销事实草稿: {}", patch.id);
            }
        },
        Commands::Draft { command } => match command {
            DraftCommand::Generate {
                path,
                candidate,
                include_pending,
                operation,
                selector,
                source_selectors,
                replacement_file,
                delete_document,
            } => {
                let mut options =
                    DraftOptions::new(resolve_path(path)?, candidate, include_pending);
                if let Some(operation) = operation {
                    let replacement_content = replacement_file
                        .map(|path| fs::read_to_string(&path).map_err(anyhow::Error::from))
                        .transpose()?;
                    options = options.with_operation(
                        operation.parse::<KnowledgeOperation>()?,
                        selector,
                        source_selectors,
                        replacement_content,
                        delete_document,
                    );
                } else if selector.is_some()
                    || !source_selectors.is_empty()
                    || replacement_file.is_some()
                    || delete_document
                {
                    anyhow::bail!(
                        "selector、source-selector、replacement-file 和 delete-document 必须与 --operation 同时使用"
                    );
                }
                let result = generate_document_drafts(options)?;
                println!("文档草稿生成完成");
                println!("草稿目录: {}", result.patches_dir.display());
                println!("草稿数量: {}", result.patches.len());

                for patch in &result.patches {
                    print_patch_summary(patch);
                }
            }
            DraftCommand::List { path } => {
                let patches = list_document_patches(resolve_path(path)?)?;
                println!("文档草稿数量: {}", patches.len());

                for patch in &patches {
                    print_patch_summary(patch);
                }
            }
            DraftCommand::Apply { id, path } => {
                let result = apply_document_patch(resolve_path(path)?, &id)?;
                println!("文档草稿已应用");
                println!("草稿文件: {}", result.patch_path.display());
                println!("目标文档: {}", result.target_doc_path.display());
                println!("草稿 ID: {}", result.patch.id);
            }
            DraftCommand::Revert { id, path } => {
                let result = revert_document_patch(resolve_path(path)?, &id)?;
                println!("文档草稿已撤销");
                println!("目标文档: {}", result.target_doc_path.display());
                println!("草稿 ID: {}", result.patch.id);
            }
        },
        Commands::Index { path } => {
            let result = index_project(resolve_path(path)?)?;
            println!("本地知识索引完成");
            println!("索引文件: {}", result.index_path.display());
            println!("索引文档数: {}", result.document_count);
        }
        Commands::Search { query, path, limit } => {
            let results = search_project(SearchOptions::new(resolve_path(path)?, query, limit))?;
            println!("搜索结果数量: {}", results.len());

            for result in &results {
                println!();
                println!("来源: {}", result.path);
                println!("类型: {:?}", result.source_type);
                println!("标题: {}", result.title);
                println!("片段: {}", result.snippet);
            }
        }
        Commands::Mcp { path } => {
            cyclaw_mcp::run_stdio_server(cyclaw_mcp::McpServerOptions::new(resolve_path(path)?))?;
        }
        Commands::Model { command } => match command {
            ModelCommand::Add {
                name,
                base_url,
                model,
                api_key_env,
                thinking,
                path,
                active,
                fallback,
            } => {
                let project_root = resolve_path(path)?;
                let result = add_provider(AddProviderOptions {
                    project_root: project_root.clone(),
                    name,
                    base_url,
                    model,
                    api_key_env,
                    thinking_enabled: thinking,
                    set_active: active,
                    fallback_provider: fallback,
                })?;
                println!("模型 Provider 已保存");
                println!("配置文件: {}", result.config_path.display());
                println!(
                    "Active Provider: {}",
                    result
                        .active_provider
                        .clone()
                        .unwrap_or_else(|| "无".to_string())
                );
                println!("Base URL: {}", result.provider.base_url);
                println!("模型: {}", result.provider.model);
                println!("API Key 环境变量: {}", result.provider.api_key_env);
                println!(
                    "思考模式: {}",
                    if result.provider.thinking_enabled {
                        "开启"
                    } else {
                        "关闭"
                    }
                );
                if let Some(fallback) = &result.provider.fallback_provider {
                    println!("备用 Provider: {}（请求失败时自动降级）", fallback);
                }
                println!("说明: API Key 不会写入项目文件，请通过环境变量提供。");
                append_cli_event(
                    &project_root,
                    cyclaw_events::AgentEventType::ModelProviderConfigured,
                    "配置模型 Provider",
                    serde_json::json!({ "active_provider": result.active_provider, "model": result.provider.model }),
                )?;
            }
            ModelCommand::List { path } => {
                let result = list_providers(resolve_path(path)?)?;
                println!("模型 Provider 配置: {}", result.config_path.display());
                println!(
                    "Active Provider: {}",
                    result.active_provider.unwrap_or_else(|| "无".to_string())
                );
                println!("Provider 数量: {}", result.providers.len());

                for (name, provider) in &result.providers {
                    println!();
                    println!("名称: {}", name);
                    println!("类型: {:?}", provider.provider_type);
                    println!("Base URL: {}", provider.base_url);
                    println!("模型: {}", provider.model);
                    println!("API Key 环境变量: {}", provider.api_key_env);
                    if let Some(fallback) = &provider.fallback_provider {
                        println!("备用 Provider: {}", fallback);
                    }
                    println!(
                        "思考模式: {}",
                        if provider.thinking_enabled {
                            "开启"
                        } else {
                            "关闭"
                        }
                    );
                }
            }
            ModelCommand::Use { name, path } => {
                let result = use_provider(resolve_path(path)?, &name)?;
                println!("活动模型 Provider 已切换");
                println!("配置文件: {}", result.config_path.display());
                println!("Active Provider: {}", result.active_provider);
            }
            ModelCommand::Test { name, prompt, path } => {
                let result = test_provider(TestProviderOptions {
                    project_root: resolve_path(path)?,
                    name,
                    prompt,
                    trace: None,
                })?;
                println!("模型 Provider 测试成功");
                println!("Provider: {}", result.provider_name);
                println!("模型: {}", result.model);
                println!("响应: {}", result.response);
                if let Some(from) = &result.fallback_from {
                    println!(
                        "注意: 主 Provider {} 请求失败，本次结果来自备用 Provider",
                        from
                    );
                }
                if let Some(trace_id) = &result.trace_id {
                    println!("链路: {}", trace_id);
                    println!("查看推理路径: cyclaw trace show {}", trace_id);
                }
            }
            ModelCommand::Calls {
                path,
                limit,
                provider,
            } => {
                let project_root = resolve_path(path)?;
                let mut calls: Vec<ModelCallRecord> = read_model_calls(&project_root)?
                    .into_iter()
                    .filter(|call| {
                        provider
                            .as_deref()
                            .map(|name| call.provider == name)
                            .unwrap_or(true)
                    })
                    .collect();
                calls.reverse();
                println!("模型调用记录数量: {}", calls.len());
                for call in calls.iter().take(limit) {
                    println!();
                    println!("ID: {}", call.id);
                    println!("时间: {} -> {}", call.started_at, call.finished_at);
                    println!("Provider: {} / 模型: {}", call.provider, call.model);
                    println!("阶段: {}", call.phase.as_deref().unwrap_or("-"));
                    println!(
                        "状态: {:?}  尝试次数: {}  延迟: {} ms",
                        call.status, call.attempts, call.latency_millis
                    );
                    println!(
                        "Tokens: 输入 {} / 输出 {} / 合计 {}",
                        call.input_tokens, call.output_tokens, call.total_tokens
                    );
                    if let Some(trace_id) = &call.trace_id {
                        println!(
                            "Trace: {} (span={})",
                            trace_id,
                            call.span_id.as_deref().unwrap_or("-")
                        );
                    }
                    if let Some(prompt) = &call.prompt_preview {
                        println!("输入预览: {}", prompt);
                    }
                    if let Some(response) = &call.response_preview {
                        println!("输出预览: {}", response);
                    }
                    if let Some(error) = &call.error_summary {
                        println!("错误: {}", error);
                    }
                }
            }
        },
        Commands::Agent { command } => match command {
            AgentCommand::Run {
                once,
                provider,
                no_model,
                path,
            } => {
                if !once {
                    anyhow::bail!("当前仅支持 `cyclaw agent run --once`");
                }
                let result = run_agent_once(AgentRunOptions::new(
                    resolve_path(path)?,
                    provider,
                    !no_model,
                ))?;
                println!("Agent 运行完成");
                println!("运行记录: {}", result.record_path.display());
                println!("运行 ID: {}", result.record.run_id);
                println!("发现变更: {}", render_bool(result.record.changed));
                println!("待处理候选知识: {}", result.record.pending_knowledge_count);
                println!("待应用文档草稿: {}", result.record.pending_patch_count);
                println!(
                    "模型 Provider: {}",
                    result.record.model_provider.as_deref().unwrap_or("未使用")
                );
                println!();
                println!("步骤:");
                for step in &result.record.steps {
                    println!("- {} [{:?}] {}", step.name, step.status, step.detail);
                }
                if let Some(summary) = &result.record.model_response_summary {
                    println!();
                    println!("模型响应摘要:");
                    println!("{}", summary);
                }
            }
            AgentCommand::Runs { command } => match command {
                AgentRunsCommand::List { path, limit } => {
                    let project_root = resolve_path(path)?;
                    let runs = list_agent_runs(&project_root, limit)?;
                    println!("Agent 运行记录数量: {}", runs.len());
                    for run in runs {
                        println!(
                            "{} | {} | changed={} | pending_knowledge={} | pending_patches={}",
                            run.run_id,
                            run.completed_at,
                            run.changed,
                            run.pending_knowledge_count,
                            run.pending_patch_count
                        );
                    }
                }
                AgentRunsCommand::Clean { path, keep } => {
                    let project_root = resolve_path(path)?;
                    let deleted = cleanup_agent_runs(&project_root, keep)?;
                    println!("已清理 Agent 运行记录: {}", deleted);
                    println!("保留数量上限: {}", keep);
                }
            },
        },
        Commands::Task { command } => match command {
            TaskCommand::Begin {
                title,
                objective,
                related_files,
                context_budget_tokens,
                path,
            } => {
                let project_root = resolve_path(path)?;
                let mut options = BeginTaskOptions::new(project_root.clone(), title, objective);
                options.related_files = related_files;
                options.context_budget_tokens = context_budget_tokens;
                let task = begin_task(options)?;
                let context = get_task_context(
                    &project_root,
                    Some(&task.id),
                    None,
                    Some(task.context_budget_tokens),
                    20,
                )?;
                println!("任务已开始: {}", task.id);
                println!("{}", serde_json::to_string_pretty(&context)?);
            }
            TaskCommand::Active { path } => {
                let task = get_active_task(&resolve_path(path)?)?;
                println!("{}", serde_json::to_string_pretty(&task)?);
            }
            TaskCommand::Context {
                task,
                query,
                budget_tokens,
                limit,
                path,
            } => {
                let context = get_task_context(
                    &resolve_path(path)?,
                    task.as_deref(),
                    query.as_deref(),
                    budget_tokens,
                    limit,
                )?;
                println!("{}", serde_json::to_string_pretty(&context)?);
            }
            TaskCommand::Decision {
                statement,
                rationale,
                evidence,
                confidence,
                task,
                path,
            } => {
                let (_, fact) = record_task_decision(
                    &resolve_path(path)?,
                    task.as_deref(),
                    statement,
                    rationale,
                    evidence,
                    confidence,
                )?;
                println!("已记录决策事实: {}", fact.id);
                println!("{}", serde_json::to_string_pretty(&fact)?);
            }
            TaskCommand::Failure {
                approach,
                reason,
                evidence,
                task,
                path,
            } => {
                let (_, fact) = record_task_failed_approach(
                    &resolve_path(path)?,
                    task.as_deref(),
                    approach,
                    reason,
                    evidence,
                )?;
                println!("已记录失败方案: {}", fact.id);
                println!("{}", serde_json::to_string_pretty(&fact)?);
            }
            TaskCommand::Checkpoint {
                summary,
                related_files,
                task,
                path,
            } => {
                let task = checkpoint_task(
                    &resolve_path(path)?,
                    task.as_deref(),
                    summary,
                    related_files,
                )?;
                println!("任务检查点已保存: {}", task.id);
                println!("检查点数量: {}", task.checkpoints.len());
            }
            TaskCommand::Reconcile { task, path } => {
                let report = reconcile_project_knowledge(&resolve_path(path)?, task)?;
                println!("知识对账完成: {}", report.id);
                println!("重复: {}", report.duplicate_count);
                println!("冲突: {}", report.conflict_count);
                println!("失效: {}", report.stale_count);
                println!("证据漂移: {}", report.drift_count);
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            TaskCommand::Close {
                summary,
                task,
                reconcile,
                path,
            } => {
                let result = close_task(&resolve_path(path)?, task.as_deref(), summary, reconcile)?;
                println!("任务已关闭: {}", result.task.id);
                println!("决策: {}", result.task.decisions.len());
                println!("失败方案: {}", result.task.failed_approaches.len());
                if let Some(report) = result.reconciliation {
                    println!("知识对账: {} 项发现", report.findings.len());
                }
            }
            TaskCommand::List { limit, path } => {
                let tasks = list_tasks(&resolve_path(path)?, limit)?;
                println!("{}", serde_json::to_string_pretty(&tasks)?);
            }
            TaskCommand::Facts { limit, path } => {
                let mut facts = list_project_facts(&resolve_path(path)?)?;
                facts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
                facts.truncate(limit.clamp(1, 200));
                println!("{}", serde_json::to_string_pretty(&facts)?);
            }
            TaskCommand::LatestReconciliation { path } => {
                let report = get_latest_reconciliation(&resolve_path(path)?)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        },
        Commands::Policy { command } => match command {
            PolicyCommand::Show { path } => {
                let project_root = resolve_path(path)?;
                let policy = load_or_default(&project_root)?;
                println!("cyClaw 权限策略");
                println!("项目根目录: {}", project_root.display());
                println!("默认权限层级: {:?}", policy.permissions.default_level);
                println!("写入范围: {}", policy.permissions.write_scopes.join(", "));
                println!("拒绝路径: {}", policy.permissions.denied_paths.join(", "));
                println!(
                    "允许模型调用: {}",
                    render_bool(policy.permissions.allow_model_call)
                );
                println!(
                    "允许联网: {}",
                    render_bool(policy.permissions.allow_network)
                );
                println!(
                    "允许应用文档: {}",
                    render_bool(policy.permissions.allow_docs_apply)
                );
                println!(
                    "模型最大上下文字符: {}",
                    policy.model_policy.max_context_chars
                );
                println!("模型最大并发: {}", policy.model_policy.max_concurrent_calls);
            }
            PolicyCommand::Check {
                target,
                level,
                path,
            } => {
                let project_root = resolve_path(path)?;
                let level = parse_permission_level(&level)?;
                let result = check_write_path(&project_root, &target, level)?;
                println!("权限检查");
                println!("目标路径: {}", result.normalized_path);
                println!("权限层级: {:?}", level);
                println!("允许: {}", render_bool(result.allowed));
                println!("原因: {}", result.reason);
            }
            PolicyCommand::Set { key, enabled, path } => {
                let project_root = resolve_path(path)?;
                let policy = set_permission(&project_root, &key, enabled)?;
                append_cli_event(
                    &project_root,
                    cyclaw_events::AgentEventType::PolicyChanged,
                    "更新权限策略",
                    serde_json::json!({ "key": key, "enabled": enabled }),
                )?;
                println!("权限策略已更新");
                println!("{}: {}", key, render_bool(enabled));
                println!(
                    "模型调用: {}",
                    render_bool(policy.permissions.allow_model_call)
                );
                println!(
                    "文档应用: {}",
                    render_bool(policy.permissions.allow_docs_apply)
                );
            }
            PolicyCommand::EnableModel { path } => {
                let project_root = resolve_path(path)?;
                set_permission(&project_root, "allow_model_call", true)?;
                let policy = set_permission(&project_root, "allow_network", true)?;
                append_cli_event(
                    &project_root,
                    cyclaw_events::AgentEventType::PolicyChanged,
                    "启用模型权限",
                    serde_json::json!({ "allow_model_call": true, "allow_network": true }),
                )?;
                println!("已启用模型调用和联网权限");
                println!(
                    "模型调用: {}",
                    render_bool(policy.permissions.allow_model_call)
                );
                println!("联网: {}", render_bool(policy.permissions.allow_network));
            }
            PolicyCommand::EnableDocsApply { path } => {
                let project_root = resolve_path(path)?;
                let policy = set_permission(&project_root, "allow_docs_apply", true)?;
                append_cli_event(
                    &project_root,
                    cyclaw_events::AgentEventType::PolicyChanged,
                    "启用文档应用权限",
                    serde_json::json!({ "allow_docs_apply": true }),
                )?;
                println!("已启用文档草稿应用权限");
                println!(
                    "文档应用: {}",
                    render_bool(policy.permissions.allow_docs_apply)
                );
            }
        },
        Commands::Events { command } => match command {
            EventsCommand::List { path, limit } => {
                let project_root = resolve_path(path)?;
                let mut events = cyclaw_events::read_events(&project_root)?;
                events.reverse();
                println!("事件数量: {}", events.len());
                for event in events.iter().take(limit) {
                    println!();
                    println!("ID: {}", event.id);
                    println!("类型: {:?}", event.event_type);
                    println!("时间: {}", event.created_at);
                    println!("来源: {}", event.source);
                    println!("摘要: {}", event.summary);
                    println!("数据: {}", event.data);
                }
            }
        },
        Commands::Trace { command } => match command {
            TraceCommand::List { path, limit } => {
                let project_root = resolve_path(path)?;
                let mut grouped: std::collections::BTreeMap<String, (String, usize, usize, u128)> =
                    std::collections::BTreeMap::new();
                for span in read_trace_spans(&project_root)? {
                    let entry = grouped
                        .entry(span.trace_id.clone())
                        .or_insert_with(|| (span.started_at.clone(), 0, 0, 0));
                    entry.1 += 1;
                    if span.status == TraceSpanStatus::Error {
                        entry.2 += 1;
                    }
                    entry.3 = entry.3.saturating_add(span.duration_millis);
                }
                let mut traces: Vec<(String, (String, usize, usize, u128))> =
                    grouped.into_iter().collect();
                traces.sort_by(|a, b| b.1.0.cmp(&a.1.0));
                println!("Trace 数量: {}", traces.len());
                for (trace_id, (started_at, span_count, error_count, total_millis)) in
                    traces.into_iter().take(limit)
                {
                    println!();
                    println!("Trace: {}", trace_id);
                    println!("  开始: {}", started_at);
                    println!("  Span 数: {}", span_count);
                    if error_count > 0 {
                        println!("  错误 Span: {}", error_count);
                    }
                    println!("  Span 总耗时: {} ms", total_millis);
                    println!("  查看链路: cyclaw trace show {}", trace_id);
                }
            }
            TraceCommand::Show { trace_id, path } => {
                let project_root = resolve_path(path)?;
                print_trace_view(&project_root, &trace_id)?;
            }
        },
        Commands::Retry { command } => match command {
            RetryCommand::List {
                path,
                limit,
                status,
            } => {
                let project_root = resolve_path(path)?;
                let status_filter: Option<RetryStatus> = match status.as_deref() {
                    Some("pending") => Some(RetryStatus::Pending),
                    Some("in_progress") => Some(RetryStatus::InProgress),
                    Some("completed") => Some(RetryStatus::Completed),
                    Some("exhausted") => Some(RetryStatus::Exhausted),
                    Some("abandoned") => Some(RetryStatus::Abandoned),
                    Some(other) => anyhow::bail!(
                        "未知重试状态: {}（可选 pending/in_progress/completed/exhausted/abandoned）",
                        other
                    ),
                    None => None,
                };
                let mut tasks: Vec<_> = read_retry_queue(&project_root)?
                    .into_iter()
                    .filter(|task| match &status_filter {
                        Some(filter) => &task.status == filter,
                        None => true,
                    })
                    .collect();
                tasks.reverse();
                println!("重试任务数量: {}", tasks.len());
                for task in tasks.iter().take(limit) {
                    println!();
                    println!("ID: {}", task.id);
                    println!("类型: {:?}  状态: {:?}", task.kind, task.status);
                    println!(
                        "尝试: {}/{}  下次重试: {}",
                        task.attempt_count,
                        task.max_attempts,
                        task.next_retry_at.as_deref().unwrap_or("-")
                    );
                    println!("Trace: {}", task.trace_id.as_deref().unwrap_or("-"));
                    println!("创建: {}  更新: {}", task.created_at, task.updated_at);
                    if let Some(error) = &task.last_error {
                        println!("最近错误: {}", error.chars().take(200).collect::<String>());
                    }
                    if task.degraded {
                        println!("已降级: {}", task.degraded_reason.as_deref().unwrap_or("-"));
                    }
                    println!(
                        "载荷: {}",
                        serde_json::to_string(&task.payload).unwrap_or_default()
                    );
                }
            }
            RetryCommand::Run { path, limit } => {
                let project_root = resolve_path(path)?;
                let result = run_due_retries(RetryRunOptions {
                    project_root,
                    limit,
                })?;
                println!(
                    "到期重试: 执行 {} / 完成 {} / 继续退避 {} / 耗尽降级 {}",
                    result.executed, result.completed, result.scheduled, result.exhausted
                );
                for outcome in &result.outcomes {
                    println!(
                        "- [{:?}] {} run={} {}",
                        outcome.status,
                        outcome.retry_id,
                        outcome.run_id.as_deref().unwrap_or("-"),
                        outcome.detail
                    );
                }
            }
            RetryCommand::Abandon { retry_id, path } => {
                let project_root = resolve_path(path)?;
                let outcome = abandon_retry_task(&project_root, &retry_id)?;
                println!("重试任务已放弃: {}", outcome.retry_id);
                println!("说明: {}", outcome.detail);
            }
        },
        Commands::Hooks { command } => match command {
            HooksCommand::Run {
                event,
                path,
                strict,
            } => run_hook_event(&resolve_path(path)?, &event, strict)?,
            HooksCommand::InstallGit { path, force } => {
                install_git_hook(&resolve_path(path)?, force)?
            }
            HooksCommand::Status { path } => print_git_hook_status(&resolve_path(path)?)?,
            HooksCommand::UninstallGit { path } => uninstall_git_hook(&resolve_path(path)?)?,
        },
    }

    Ok(())
}

fn resolve_path(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    Ok(path.canonicalize()?)
}

fn truncate_output(output: &str) -> String {
    output.chars().take(2000).collect()
}

/// 按 trace_id 还原完整链路：span 树 + 关联的大模型推理明细与执行事件。
fn print_trace_view(project_root: &Path, trace_id: &str) -> Result<()> {
    let mut spans: Vec<TraceSpan> = read_trace_spans(project_root)?
        .into_iter()
        .filter(|span| span.trace_id == trace_id)
        .collect();
    if spans.is_empty() {
        println!("未找到 Trace: {}", trace_id);
        println!("提示: agent run 的 run_id、exec/model test 的 trace 均可查询。");
        return Ok(());
    }
    spans.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then(a.span_id.cmp(&b.span_id))
    });

    let model_calls: Vec<ModelCallRecord> = read_model_calls(project_root)?
        .into_iter()
        .filter(|call| call.trace_id.as_deref() == Some(trace_id))
        .collect();
    let executions: Vec<cyclaw_events::ExecutionEvent> = read_execution_events(project_root)?
        .into_iter()
        .filter(|event| event.trace_id.as_deref() == Some(trace_id))
        .collect();

    let known: std::collections::BTreeSet<&str> =
        spans.iter().map(|span| span.span_id.as_str()).collect();
    let mut children: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        match span.parent_span_id.as_deref() {
            Some(parent) if known.contains(parent) => {
                children.entry(parent).or_default().push(index);
            }
            _ => roots.push(index),
        }
    }

    println!("Trace: {}", trace_id);
    println!("Span 数量: {}", spans.len());
    println!();
    let mut printed = vec![false; spans.len()];
    fn print_span_tree(
        spans: &[TraceSpan],
        children: &std::collections::BTreeMap<&str, Vec<usize>>,
        index: usize,
        depth: usize,
        printed: &mut [bool],
    ) {
        if printed[index] {
            return;
        }
        printed[index] = true;
        let span = &spans[index];
        println!(
            "{}{} [{:?}] {} ms (span_id={})",
            "  ".repeat(depth),
            span.name,
            span.status,
            span.duration_millis,
            span.span_id
        );
        if let Some(parent) = &span.parent_span_id {
            println!("{}  父 span: {}", "  ".repeat(depth), parent);
        }
        println!(
            "{}  时间: {} -> {}  属性: {}",
            "  ".repeat(depth),
            span.started_at,
            span.ended_at,
            serde_json::to_string(&span.attributes).unwrap_or_default()
        );
        if let Some(child_indexes) = children.get(span.span_id.as_str()) {
            for &child_index in child_indexes {
                print_span_tree(spans, children, child_index, depth + 1, printed);
            }
        }
    }
    for root in roots {
        print_span_tree(&spans, &children, root, 0, &mut printed);
    }
    for (index, span) in spans.iter().enumerate() {
        if !printed[index] {
            // 父 span 不在本 trace 中的孤儿节点兜底输出。
            println!(
                "{} [{:?}] {} ms (span_id={})",
                span.name, span.status, span.duration_millis, span.span_id
            );
        }
    }

    if !model_calls.is_empty() {
        println!();
        println!("大模型推理路径:");
        for call in model_calls {
            println!(
                "- [{:?}] {} / {}  phase={}  尝试={}  延迟={} ms  tokens={}/{}",
                call.status,
                call.provider,
                call.model,
                call.phase.as_deref().unwrap_or("-"),
                call.attempts,
                call.latency_millis,
                call.input_tokens,
                call.output_tokens,
            );
            if let Some(prompt) = &call.prompt_preview {
                println!("  输入预览: {}", prompt);
            }
            if let Some(response) = &call.response_preview {
                println!("  输出预览: {}", response);
            }
            if let Some(error) = &call.error_summary {
                println!("  错误: {}", error);
            }
        }
    }

    if !executions.is_empty() {
        println!();
        println!("关联执行事件:");
        for event in executions {
            println!(
                "- [{:?}] {}  exit_code={:?}  timed_out={}  duration={:?} ms",
                event.kind,
                event.command_summary,
                event.exit_code,
                event.timed_out,
                event.duration_millis,
            );
        }
    }

    Ok(())
}

fn git_head(project_root: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn observer_task_name(project_root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    project_root.to_string_lossy().hash(&mut hasher);
    format!("cyClaw Observer {:016x}", hasher.finish())
}

fn install_observer_task(project_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let executable = std::env::current_exe()?;
        let task_name = observer_task_name(project_root);
        let command = format!(
            "\"{}\" observer run --path \"{}\"",
            executable.display(),
            project_root.display()
        );
        let output = Command::new("schtasks")
            .args([
                "/Create", "/TN", &task_name, "/TR", &command, "/SC", "ONLOGON", "/F",
            ])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "无法安装 Observer 计划任务: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let start = Command::new("schtasks")
            .args(["/Run", "/TN", &task_name])
            .output()?;
        if !start.status.success() {
            anyhow::bail!(
                "Observer 任务已安装但无法立即启动: {}",
                String::from_utf8_lossy(&start.stderr).trim()
            );
        }
        println!("已安装并启动独立 Observer: {}", task_name);
        println!("项目: {}", project_root.display());
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = project_root;
        anyhow::bail!(
            "当前版本仅提供 Windows 任务计划安装；请以服务管理器运行 `cyclaw observer run`"
        );
    }
}

fn uninstall_observer_task(project_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let task_name = observer_task_name(project_root);
        let output = Command::new("schtasks")
            .args(["/Delete", "/TN", &task_name, "/F"])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "无法卸载 Observer 计划任务: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        println!("已卸载独立 Observer: {}", task_name);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = project_root;
        anyhow::bail!("当前版本仅提供 Windows 任务计划卸载");
    }
}

fn new_fact(
    statement: String,
    fact_type: FactType,
    evidence: EvidenceOptions,
    confidence: u8,
) -> Result<ProjectFact> {
    let (paths, details) = evidence.into_parts()?;
    Ok(project_fact_from_input(FactInput {
        statement,
        fact_type,
        evidence: paths,
        evidence_details: details,
        source_task_id: None,
        confidence,
        valid_from: None,
        valid_until: None,
    }))
}

fn find_project_fact(project_root: &Path, fact_id: &str) -> Result<ProjectFact> {
    list_project_facts(project_root)?
        .into_iter()
        .find(|fact| fact.id == fact_id)
        .ok_or_else(|| anyhow::anyhow!("未找到事实: {}", fact_id))
}

fn finish_fact_preview(project_root: &Path, patch: FactPatch, apply: bool) -> Result<()> {
    println!("事实草稿: {}", patch.id);
    println!("操作: {:?}", patch.operation);
    println!("预览指纹: {}", patch.preview_fingerprint);
    if apply {
        let patch = apply_fact_patch(project_root, &patch.id)?;
        println!(
            "已应用事实草稿: {}；可使用 `cyclaw fact revert {}` 撤销",
            patch.id, patch.id
        );
    } else {
        println!(
            "Fact Ledger 未修改；使用 `cyclaw fact apply {}` 应用",
            patch.id
        );
    }
    Ok(())
}

const CYCLAW_HOOK_MARKER: &str = "# managed-by-cyclaw";

fn run_hook_event(project_root: &Path, event: &str, strict: bool) -> Result<()> {
    match event {
        "session-start" => {
            let status = project_status(project_root.to_path_buf())?;
            println!("cyClaw Session Start");
            println!("待处理候选: {}", status.inbox_pending);
            println!("待应用草稿: {}", status.draft_pending);
            for step in status.suggested_next_steps.iter().take(3) {
                println!("- {}", step);
            }
        }
        "session-stop" | "pre-commit" => {
            let diff_options = if event == "pre-commit" {
                DiffOptions::new(project_root.to_path_buf())
                    .incremental(staged_git_paths(project_root)?)
            } else {
                DiffOptions::new(project_root.to_path_buf())
            };
            let diff = analyze_project_diff(diff_options)?;
            let inbox = generate_inbox(InboxGenerateOptions::new(
                project_root.to_path_buf(),
                Some(diff.analysis_path),
            ))?;
            let high_pending = inbox
                .generated
                .into_iter()
                .filter(|candidate| {
                    candidate.status == KnowledgeStatus::Pending && candidate.confidence >= 80
                })
                .count();
            println!("cyClaw Hook: {}", event);
            println!("变化文件: {}", diff.analysis.summary.total_files);
            println!("新增候选: {}", inbox.added.len());
            println!("本轮高置信度待处理: {}", high_pending);
            append_cli_event(
                project_root,
                cyclaw_events::AgentEventType::HookRun,
                "运行 cyClaw Hook",
                serde_json::json!({
                    "event": event,
                    "strict": strict,
                    "changed_files": diff.analysis.summary.total_files,
                    "new_candidates": inbox.added.len(),
                    "high_confidence_pending": high_pending,
                    "blocked": strict && high_pending > 0,
                }),
            )?;
            if strict && high_pending > 0 {
                println!("严格模式不再因推断型知识候选阻止提交；候选已交由 Observer 治理。");
            }
            return Ok(());
        }
        _ => anyhow::bail!("未知 Hook 事件: {}", event),
    }
    append_cli_event(
        project_root,
        cyclaw_events::AgentEventType::HookRun,
        "运行 cyClaw Hook",
        serde_json::json!({ "event": event, "strict": strict }),
    )?;
    Ok(())
}

fn staged_git_paths(project_root: &Path) -> Result<std::collections::BTreeSet<String>> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only", "--diff-filter=ACDMRTUXB"])
        .current_dir(project_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "无法读取 staged 文件: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('\\', "/"))
        .collect())
}

fn install_git_hook(project_root: &Path, force: bool) -> Result<()> {
    let hooks_dir = resolve_git_hooks_dir(project_root)?;
    fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("pre-commit");
    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path).unwrap_or_default();
        if !force && !existing.contains(CYCLAW_HOOK_MARKER) {
            anyhow::bail!("pre-commit 已存在且不属于 cyClaw；使用 --force 明确覆盖");
        }
    }
    let executable = std::env::current_exe()?
        .to_string_lossy()
        .replace('\\', "/");
    let script = format!(
        "#!/bin/sh\n{}\nrepo_root=\"$(git rev-parse --show-toplevel)\"\nstrict=\"\"\nif [ \"$CYCLAW_HOOK_STRICT\" = \"1\" ]; then strict=\"--strict\"; fi\n\"{}\" hooks run pre-commit --path \"$repo_root\" $strict\n",
        CYCLAW_HOOK_MARKER, executable
    );
    fs::write(&hook_path, script)?;
    println!("已安装 cyClaw Git Hook: {}", hook_path.display());
    println!("默认仅提示；设置 CYCLAW_HOOK_STRICT=1 可阻止高置信度知识未处理的提交。");
    Ok(())
}

fn print_git_hook_status(project_root: &Path) -> Result<()> {
    let path = resolve_git_hooks_dir(project_root)?.join("pre-commit");
    let installed = path.exists()
        && fs::read_to_string(&path)
            .unwrap_or_default()
            .contains(CYCLAW_HOOK_MARKER);
    println!("cyClaw Git Hook: {}", render_bool(installed));
    println!("路径: {}", path.display());
    Ok(())
}

fn uninstall_git_hook(project_root: &Path) -> Result<()> {
    let path = resolve_git_hooks_dir(project_root)?.join("pre-commit");
    if !path.exists() {
        println!("cyClaw Git Hook 未安装");
        return Ok(());
    }
    let content = fs::read_to_string(&path)?;
    if !content.contains(CYCLAW_HOOK_MARKER) {
        anyhow::bail!("现有 pre-commit 不属于 cyClaw，拒绝删除");
    }
    fs::remove_file(&path)?;
    println!("已卸载 cyClaw Git Hook");
    Ok(())
}

fn resolve_git_hooks_dir(project_root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(project_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("当前目录不是 Git 仓库: {}", project_root.display());
    }
    let raw_path = String::from_utf8(output.stdout)?.trim().to_string();
    if raw_path.is_empty() {
        anyhow::bail!("Git 未返回 Hooks 目录: {}", project_root.display());
    }
    let hooks_dir = PathBuf::from(raw_path);
    if hooks_dir.is_absolute() {
        Ok(hooks_dir)
    } else {
        Ok(project_root.join(hooks_dir))
    }
}

fn print_candidate_summary(candidate: &cyclaw_knowledge::KnowledgeCandidate) {
    println!();
    println!("ID: {}", candidate.id);
    println!("摘要: {}", candidate.summary);
    println!("重要性: {}", render_importance(&candidate.importance));
    println!("状态: {:?}", candidate.status);
    println!("建议文档: {}", candidate.recommended_doc);
    println!("关联文件: {}", candidate.related_files.join(", "));
}

fn render_importance(importance: &KnowledgeImportance) -> &'static str {
    match importance {
        KnowledgeImportance::High => "高",
        KnowledgeImportance::Medium => "中",
        KnowledgeImportance::Low => "低",
    }
}

fn render_bool(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}

fn parse_permission_level(value: &str) -> Result<PermissionLevel> {
    match value {
        "read_only" => Ok(PermissionLevel::ReadOnly),
        "local_knowledge_write" => Ok(PermissionLevel::LocalKnowledgeWrite),
        "docs_write" => Ok(PermissionLevel::DocsWrite),
        "model_call" => Ok(PermissionLevel::ModelCall),
        "automation" => Ok(PermissionLevel::Automation),
        "shell" => Ok(PermissionLevel::Shell),
        _ => anyhow::bail!("未知权限层级: {}", value),
    }
}

fn append_cli_event(
    project_root: &std::path::Path,
    event_type: cyclaw_events::AgentEventType,
    summary: &str,
    data: serde_json::Value,
) -> Result<()> {
    cyclaw_events::append_event(
        project_root,
        &cyclaw_events::new_event(event_type, "cyclaw-cli", summary, data),
    )?;
    Ok(())
}

fn print_watch_tick(tick: cyclaw_core::WatchTick) {
    if tick.changed {
        if let Some(result) = tick.diff_result {
            println!("发现项目变更，已生成变更分析");
            println!("运行 ID: {}", result.run_id);
            println!("分析文件: {}", result.analysis_path.display());
            println!("变更文件: {}", result.analysis.summary.total_files);
            println!("受影响知识资产: {}", result.analysis.impacted_assets.len());
        }
        if let Some(inbox_result) = tick.inbox_result {
            println!("分析得到候选知识: {}", inbox_result.generated.len());
            println!("本次新增候选知识: {}", inbox_result.added.len());
            println!("已存在候选知识: {}", inbox_result.existing_count);
            println!("待处理候选知识: {}", inbox_result.total_pending);
            println!("知识收件箱: {}", inbox_result.inbox_path.display());
        }
        if tick.auto_applied_patches > 0 {
            println!(
                "自动管理文档: 已应用 {} 个文档草稿",
                tick.auto_applied_patches
            );
        }
    } else {
        println!("未发现新的项目变更");
    }
}

fn print_observer_tick(tick: &cyclaw_core::ObserverTick) {
    println!(
        "Observer 补偿结果: 初始化={} 扫描={} 索引={} 变更={} 验证事实={}",
        render_bool(tick.initialized),
        render_bool(tick.scanned),
        render_bool(tick.indexed),
        render_bool(tick.watch.changed),
        tick.verified_facts
    );
    if let Some(inbox) = &tick.watch.inbox_result {
        println!("新增待验证候选: {}", inbox.added.len());
    }
    if let Some(report) = &tick.reconciliation {
        println!(
            "知识对账: 重复 {}，冲突 {}，失效 {}，漂移 {}",
            report.duplicate_count, report.conflict_count, report.stale_count, report.drift_count
        );
    }
    println!("观察器状态: {}", tick.state_path.display());
}

fn is_relevant_watch_event(project_root: &Path, event: &Event) -> bool {
    event.paths.iter().any(|path| {
        let relative = path.strip_prefix(project_root).unwrap_or(path);
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let execution_report = normalized.contains("surefire-reports")
            || normalized.contains("test-results")
            || normalized.contains("junit")
            || normalized.contains("reports/tests");
        let ignored_component = relative.components().any(|component| {
            matches!(
                component.as_os_str().to_string_lossy().as_ref(),
                ".git" | ".cyclaw" | "node_modules" | "target" | "build" | "dist" | ".gradle"
            )
        });
        (execution_report || !ignored_component) && !normalized.starts_with("apps/vscode/bin/")
    })
}

fn print_patch_summary(patch: &cyclaw_docs::DocumentPatch) {
    println!();
    println!("ID: {}", patch.id);
    println!("候选知识: {}", patch.candidate_id);
    println!("目标文档: {}", patch.target_doc);
    println!("知识操作: {}", patch.operation.as_str());
    println!("状态: {:?}", patch.status);
    println!("摘要: {}", patch.summary);
    println!("{}", patch.preview);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("应能运行 Git");
        assert!(status.success());
    }

    fn init_repository() -> TempDir {
        let directory = tempfile::tempdir().expect("应能创建临时目录");
        run_git(directory.path(), &["init", "--quiet"]);
        directory
    }

    #[test]
    fn installs_and_uninstalls_managed_git_hook() {
        let repository = init_repository();
        let hooks_dir = resolve_git_hooks_dir(repository.path()).expect("应能解析 Hooks 目录");

        install_git_hook(repository.path(), false).expect("应能安装 Hook");
        let hook_path = hooks_dir.join("pre-commit");
        assert!(
            fs::read_to_string(&hook_path)
                .expect("应能读取 Hook")
                .contains(CYCLAW_HOOK_MARKER)
        );

        uninstall_git_hook(repository.path()).expect("应能卸载 Hook");
        assert!(!hook_path.exists());
    }

    #[test]
    fn refuses_to_overwrite_unmanaged_git_hook() {
        let repository = init_repository();
        let hooks_dir = resolve_git_hooks_dir(repository.path()).expect("应能解析 Hooks 目录");
        fs::create_dir_all(&hooks_dir).expect("应能创建 Hooks 目录");
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\nexit 0\n").expect("应能写入已有 Hook");

        let error = install_git_hook(repository.path(), false).expect_err("应拒绝覆盖已有 Hook");
        assert!(error.to_string().contains("不属于 cyClaw"));
    }

    #[test]
    fn resolves_worktree_hooks_directory() {
        let repository = init_repository();
        run_git(
            repository.path(),
            &["config", "user.email", "cyclaw@example.com"],
        );
        run_git(repository.path(), &["config", "user.name", "cyClaw Test"]);
        fs::write(repository.path().join("README.md"), "# test\n").expect("应能写入测试文件");
        run_git(repository.path(), &["add", "README.md"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "init"]);

        let worktree_parent = tempfile::tempdir().expect("应能创建 worktree 父目录");
        let worktree = worktree_parent.path().join("linked");
        run_git(
            repository.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                worktree.to_str().expect("路径应为 UTF-8"),
            ],
        );

        let hooks_dir = resolve_git_hooks_dir(&worktree).expect("应能解析 worktree Hooks 目录");
        assert!(hooks_dir.ends_with("hooks"));
        install_git_hook(&worktree, false).expect("应能在 worktree 中安装 Hook");
        assert!(hooks_dir.join("pre-commit").exists());
    }

    #[test]
    fn reads_only_staged_paths_for_pre_commit() {
        let repository = init_repository();
        fs::write(repository.path().join("staged.rs"), "fn staged() {}\n")
            .expect("应能写入 staged 文件");
        fs::write(repository.path().join("unstaged.rs"), "fn unstaged() {}\n")
            .expect("应能写入 unstaged 文件");
        run_git(repository.path(), &["add", "staged.rs"]);

        let paths = staged_git_paths(repository.path()).expect("应能读取 staged 文件");
        assert!(paths.contains("staged.rs"));
        assert!(!paths.contains("unstaged.rs"));
    }

    #[test]
    fn parses_fact_transactions_command() {
        let cli = Cli::try_parse_from(["cyclaw", "fact", "transactions", "--path", "."])
            .expect("应能解析事实事务诊断命令");
        assert!(matches!(
            cli.command,
            Commands::Fact {
                command: FactCommand::Transactions { .. }
            }
        ));
    }
}
