use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cyclaw_agent::{AgentRunOptions, cleanup_agent_runs, list_agent_runs, run_agent_once};
use cyclaw_core::{
    BeginTaskOptions, DiffOptions, DraftOptions, InboxGenerateOptions, InitOptions, ScanOptions,
    SearchOptions, analyze_project_diff, apply_document_patch, begin_task, checkpoint_task,
    close_task, current_change_snapshot, generate_document_drafts, generate_inbox, get_active_task,
    get_latest_reconciliation, get_task_context, index_project, init_project,
    list_document_patches, list_inbox, list_project_facts, list_tasks, project_status,
    reconcile_project_knowledge, record_task_decision, record_task_failed_approach,
    revert_document_patch, scan_project, search_project, update_inbox_status, watch_project_once,
};
use cyclaw_docs::KnowledgeOperation;
use cyclaw_knowledge::{KnowledgeImportance, KnowledgeStatus};
use cyclaw_model::{
    AddProviderOptions, TestProviderOptions, add_provider, list_providers, test_provider,
    use_provider,
};
use cyclaw_policy::{
    PermissionLevel, check_write_path, load_or_default, set_auto_apply_min_confidence,
    set_permission,
};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

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
        /// 权限键：allow_model_call/allow_network/allow_shell/allow_code_write/allow_docs_apply
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
    /// 启用自动生成并应用文档的高风险权限
    EnableAutoDocs {
        /// 项目根目录，默认使用当前目录
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
    /// 设置自动写入文档所需的最低置信度
    SetAutoThreshold {
        /// 置信度阈值，范围 0-100
        confidence: u8,
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
        Commands::Watch {
            path,
            interval,
            debounce_ms,
            once,
        } => {
            let project_root = resolve_path(path)?;
            println!("cyClaw 本地知识智能体已启动");
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
                })?;
                println!("模型 Provider 测试成功");
                println!("Provider: {}", result.provider_name);
                println!("模型: {}", result.model);
                println!("响应: {}", result.response);
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
                if let Some(response) = &result.record.model_response {
                    println!();
                    println!("模型建议:");
                    println!("{}", response);
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
                    "允许 Shell: {}",
                    render_bool(policy.permissions.allow_shell)
                );
                println!(
                    "允许写源码: {}",
                    render_bool(policy.permissions.allow_code_write)
                );
                println!(
                    "允许应用文档: {}",
                    render_bool(policy.permissions.allow_docs_apply)
                );
                println!(
                    "允许自动管理文档: {}",
                    render_bool(policy.permissions.allow_auto_apply_docs)
                );
                println!(
                    "自动写入最低置信度: {}",
                    policy.automation.auto_apply_min_confidence
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
            PolicyCommand::EnableAutoDocs { path } => {
                let project_root = resolve_path(path)?;
                set_permission(&project_root, "allow_docs_apply", true)?;
                let policy = set_permission(&project_root, "allow_auto_apply_docs", true)?;
                append_cli_event(
                    &project_root,
                    cyclaw_events::AgentEventType::PolicyChanged,
                    "启用自动文档管理权限",
                    serde_json::json!({ "allow_docs_apply": true, "allow_auto_apply_docs": true }),
                )?;
                println!("已启用自动文档管理");
                println!(
                    "文档写入: {}",
                    render_bool(policy.permissions.allow_docs_apply)
                );
                println!(
                    "自动管理文档: {}",
                    render_bool(policy.permissions.allow_auto_apply_docs)
                );
            }
            PolicyCommand::SetAutoThreshold { confidence, path } => {
                let project_root = resolve_path(path)?;
                let policy = set_auto_apply_min_confidence(&project_root, confidence)?;
                println!(
                    "自动写入最低置信度已设置为: {}",
                    policy.automation.auto_apply_min_confidence
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
                anyhow::bail!("存在 {} 个高置信度知识候选尚未处理", high_pending);
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

fn is_relevant_watch_event(project_root: &Path, event: &Event) -> bool {
    event.paths.iter().any(|path| {
        let relative = path.strip_prefix(project_root).unwrap_or(path);
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let ignored_component = relative.components().any(|component| {
            matches!(
                component.as_os_str().to_string_lossy().as_ref(),
                ".git" | ".cyclaw" | "node_modules" | "target" | "build" | "dist" | ".gradle"
            )
        });
        !ignored_component && !normalized.starts_with("apps/vscode/bin/")
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
}
