use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cyclaw_core::{
    DiffOptions, InitOptions, ScanOptions, analyze_project_diff, init_project, scan_project,
};

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
    /// 分析 Git 变更并生成知识资产影响报告
    Diff {
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
