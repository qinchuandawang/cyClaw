use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cyclaw_core::{
    DiffOptions, DraftOptions, InboxGenerateOptions, InitOptions, ScanOptions, SearchOptions,
    analyze_project_diff, apply_document_patch, current_change_fingerprint,
    generate_document_drafts, generate_inbox, index_project, init_project, list_document_patches,
    list_inbox, project_status, scan_project, search_project, update_inbox_status,
    watch_project_once,
};
use cyclaw_knowledge::{KnowledgeImportance, KnowledgeStatus};

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
        /// 轮询间隔秒数
        #[arg(short, long, default_value_t = 3)]
        interval: u64,
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
            once,
        } => {
            let project_root = resolve_path(path)?;
            println!("cyClaw 本地知识智能体已启动");
            println!("项目根目录: {}", project_root.display());
            println!("监听模式: Git 状态轮询");
            println!("轮询间隔: {} 秒", interval);

            if once {
                let tick = watch_project_once(&project_root, None)?;
                print_watch_tick(tick);
                return Ok(());
            }

            let mut last_fingerprint = current_change_fingerprint(&project_root)?;

            loop {
                thread::sleep(Duration::from_secs(interval));
                let tick = watch_project_once(&project_root, Some(&last_fingerprint))?;
                let next_fingerprint = current_change_fingerprint(&project_root)?;

                print_watch_tick(tick);

                last_fingerprint = next_fingerprint;
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
            } => {
                let result = generate_document_drafts(DraftOptions::new(
                    resolve_path(path)?,
                    candidate,
                    include_pending,
                ))?;
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
            println!("已生成候选知识: {}", inbox_result.generated.len());
            println!("待处理候选知识: {}", inbox_result.total_pending);
            println!("知识收件箱: {}", inbox_result.inbox_path.display());
        }
    } else {
        println!("未发现新的项目变更");
    }
}

fn print_patch_summary(patch: &cyclaw_docs::DocumentPatch) {
    println!();
    println!("ID: {}", patch.id);
    println!("候选知识: {}", patch.candidate_id);
    println!("目标文档: {}", patch.target_doc);
    println!("状态: {:?}", patch.status);
    println!("摘要: {}", patch.summary);
    println!("{}", patch.preview);
}
