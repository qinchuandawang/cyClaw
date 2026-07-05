use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_change_radar::{ChangeAnalysis, analyze_git_changes};
use cyclaw_scanner::{ProjectProfile, scan_project_profile};
use serde::{Deserialize, Serialize};

const CYCLE_DIR: &str = ".cyclaw";

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub project_root: PathBuf,
}

impl InitOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub project_root: PathBuf,
}

impl ScanOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

#[derive(Debug, Clone)]
pub struct DiffOptions {
    pub project_root: PathBuf,
}

impl DiffOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

#[derive(Debug, Clone)]
pub struct InitResult {
    pub project_root: PathBuf,
    pub config_path: PathBuf,
    pub profile_path: PathBuf,
    pub project_doc_path: PathBuf,
    pub profile: ProjectProfile,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub project_root: PathBuf,
    pub profile_path: PathBuf,
    pub project_doc_path: PathBuf,
    pub profile: ProjectProfile,
}

#[derive(Debug, Clone)]
pub struct DiffResult {
    pub project_root: PathBuf,
    pub run_id: String,
    pub run_dir: PathBuf,
    pub analysis_path: PathBuf,
    pub analysis: ChangeAnalysis,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CyclawConfig {
    pub schema_version: u32,
    pub project_root: String,
    pub created_at: String,
    pub scan: ScanConfig,
    pub permissions: PermissionConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ScanConfig {
    pub ignored_dirs: Vec<String>,
    pub docs_dir: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PermissionConfig {
    pub read_scope: String,
    pub write_scopes: Vec<String>,
    pub allow_network: bool,
    pub allow_shell: bool,
}

pub fn init_project(options: InitOptions) -> Result<InitResult> {
    let project_root = options.project_root;
    ensure_directory(&project_root)?;

    let cyclaw_dir = project_root.join(CYCLE_DIR);
    fs::create_dir_all(&cyclaw_dir)
        .with_context(|| format!("无法创建 cyClaw 目录: {}", cyclaw_dir.display()))?;

    let config_path = cyclaw_dir.join("config.yaml");
    if !config_path.exists() {
        let config = default_config(&project_root);
        let yaml = serde_yaml::to_string(&config)?;
        fs::write(&config_path, yaml)
            .with_context(|| format!("无法写入配置文件: {}", config_path.display()))?;
    }

    let profile = scan_project_profile(&project_root)?;
    let profile_path = write_project_profile(&project_root, &profile)?;
    let project_doc_path = write_project_doc(&project_root, &profile)?;

    Ok(InitResult {
        project_root,
        config_path,
        profile_path,
        project_doc_path,
        profile,
    })
}

pub fn scan_project(options: ScanOptions) -> Result<ScanResult> {
    let project_root = options.project_root;
    ensure_directory(&project_root)?;

    let cyclaw_dir = project_root.join(CYCLE_DIR);
    fs::create_dir_all(&cyclaw_dir)
        .with_context(|| format!("无法创建 cyClaw 目录: {}", cyclaw_dir.display()))?;

    let profile = scan_project_profile(&project_root)?;
    let profile_path = write_project_profile(&project_root, &profile)?;
    let project_doc_path = write_project_doc(&project_root, &profile)?;

    Ok(ScanResult {
        project_root,
        profile_path,
        project_doc_path,
        profile,
    })
}

pub fn analyze_project_diff(options: DiffOptions) -> Result<DiffResult> {
    let project_root = options.project_root;
    ensure_directory(&project_root)?;

    let cyclaw_dir = project_root.join(CYCLE_DIR);
    let runs_dir = cyclaw_dir.join("runs");
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("无法创建 runs 目录: {}", runs_dir.display()))?;

    let analysis = analyze_git_changes(&project_root)?;
    let run_id = format!("diff-{}", Utc::now().format("%Y%m%d%H%M%S"));
    let run_dir = runs_dir.join(&run_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("无法创建 run 目录: {}", run_dir.display()))?;

    let analysis_path = run_dir.join("change-analysis.json");
    let json = serde_json::to_string_pretty(&analysis)?;
    fs::write(&analysis_path, json)
        .with_context(|| format!("无法写入变更分析: {}", analysis_path.display()))?;

    Ok(DiffResult {
        project_root,
        run_id,
        run_dir,
        analysis_path,
        analysis,
    })
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!("项目根目录不存在或不是目录: {}", path.display());
    }
    Ok(())
}

fn default_config(project_root: &Path) -> CyclawConfig {
    CyclawConfig {
        schema_version: 1,
        project_root: project_root.display().to_string(),
        created_at: Utc::now().to_rfc3339(),
        scan: ScanConfig {
            ignored_dirs: vec![
                ".git".to_string(),
                ".cyclaw".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string(),
                "build".to_string(),
                ".next".to_string(),
                ".venv".to_string(),
                "__pycache__".to_string(),
            ],
            docs_dir: "docs".to_string(),
        },
        permissions: PermissionConfig {
            read_scope: "project".to_string(),
            write_scopes: vec!["docs".to_string(), ".cyclaw".to_string()],
            allow_network: false,
            allow_shell: false,
        },
    }
}

fn write_project_profile(project_root: &Path, profile: &ProjectProfile) -> Result<PathBuf> {
    let path = project_root.join(CYCLE_DIR).join("project-profile.json");
    let json = serde_json::to_string_pretty(profile)?;
    fs::write(&path, json).with_context(|| format!("无法写入项目画像: {}", path.display()))?;
    Ok(path)
}

fn write_project_doc(project_root: &Path, profile: &ProjectProfile) -> Result<PathBuf> {
    let path = project_root.join(CYCLE_DIR).join("project.md");
    let content = render_project_doc(profile);
    fs::write(&path, content).with_context(|| format!("无法写入项目说明: {}", path.display()))?;
    Ok(path)
}

fn render_project_doc(profile: &ProjectProfile) -> String {
    let languages = render_list(&profile.languages);
    let frameworks = render_list(&profile.frameworks);
    let dependencies = render_named_paths(&profile.dependency_files);
    let docs = render_named_paths(&profile.document_paths);
    let configs = render_named_paths(&profile.config_files);

    format!(
        r#"# cyClaw 项目说明

本文件由 `cyclaw scan` 自动生成，用于帮助人和 AI 快速理解项目知识入口。

## 项目概览

- 项目根目录：`{}`
- 是否 Git 项目：`{}`
- 当前分支：`{}`
- 最近扫描时间：`{}`

## 识别语言

{}

## 识别框架

{}

## 依赖文件

{}

## 文档入口

{}

## 配置文件

{}

## 后续建议

- 如果本次代码变更涉及 API，请同步检查 `docs/api.md`。
- 如果本次代码变更涉及数据模型，请同步检查 `docs/schema.md`。
- 如果本次代码变更涉及依赖版本，请同步检查 `docs/dependencies.md`。
- 如果本次代码变更涉及环境变量或部署参数，请同步检查 `docs/environment.md`。
"#,
        profile.project_root,
        if profile.is_git_repository {
            "是"
        } else {
            "否"
        },
        profile.git_branch.as_deref().unwrap_or("未知"),
        profile.scanned_at,
        languages,
        frameworks,
        dependencies,
        docs,
        configs
    )
}

fn render_list(values: &[String]) -> String {
    if values.is_empty() {
        return "- 未识别".to_string();
    }

    values
        .iter()
        .map(|value| format!("- {}", value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_named_paths(values: &[cyclaw_scanner::PathRecord]) -> String {
    if values.is_empty() {
        return "- 未识别".to_string();
    }

    values
        .iter()
        .map(|value| format!("- `{}`：{}", value.path, value.kind))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn default_config_is_local_first() {
        let config = default_config(Path::new("/tmp/project"));

        assert!(!config.permissions.allow_network);
        assert!(!config.permissions.allow_shell);
        assert!(
            config
                .permissions
                .write_scopes
                .contains(&"docs".to_string())
        );
        assert!(
            config
                .permissions
                .write_scopes
                .contains(&".cyclaw".to_string())
        );
    }

    #[test]
    fn analyze_project_diff_writes_change_analysis() {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.email", "test@example.com"]);
        run_git(temp.path(), &["config", "user.name", "Test User"]);

        fs::write(
            temp.path().join("package.json"),
            r#"{"dependencies":{"react":"latest"}}"#,
        )
        .unwrap();
        run_git(temp.path(), &["add", "."]);
        run_git(temp.path(), &["commit", "-m", "initial"]);

        fs::write(
            temp.path().join("package.json"),
            r#"{"dependencies":{"react":"latest","vite":"latest"}}"#,
        )
        .unwrap();
        fs::write(temp.path().join(".env.example"), "API_URL=http://localhost").unwrap();

        let result = analyze_project_diff(DiffOptions::new(temp.path().to_path_buf())).unwrap();

        assert!(result.analysis_path.exists());
        assert_eq!(result.analysis.summary.dependency_changes, 1);
        assert_eq!(result.analysis.summary.environment_changes, 1);
        assert!(
            result
                .analysis
                .impacted_assets
                .iter()
                .any(|asset| asset.asset == "docs/dependencies.md")
        );
        assert!(
            result
                .analysis
                .impacted_assets
                .iter()
                .any(|asset| asset.asset == "docs/environment.md")
        );
    }

    fn run_git(project_root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(project_root)
            .args(args)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "git {} 执行失败: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
