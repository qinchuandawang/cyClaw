use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeAnalysis {
    pub schema_version: u32,
    pub project_root: String,
    pub analyzed_at: String,
    pub git_branch: Option<String>,
    pub git_head: Option<String>,
    pub has_changes: bool,
    pub changed_files: Vec<ChangedFile>,
    pub impacted_assets: Vec<ImpactedAsset>,
    pub summary: ChangeSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: GitFileStatus,
    pub change_types: Vec<ChangeType>,
    pub suggested_docs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    TypeChanged,
    Unmerged,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Api,
    Schema,
    Dependency,
    Config,
    Environment,
    Architecture,
    Documentation,
    Source,
    Test,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImpactedAsset {
    pub asset: String,
    pub reason: String,
    pub related_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    pub total_files: usize,
    pub api_changes: usize,
    pub schema_changes: usize,
    pub dependency_changes: usize,
    pub config_changes: usize,
    pub environment_changes: usize,
    pub documentation_changes: usize,
    pub architecture_changes: usize,
}

pub fn analyze_git_changes(project_root: &Path) -> Result<ChangeAnalysis> {
    ensure_git_repository(project_root)?;

    let status_output = run_git(project_root, &["status", "--porcelain"])?;
    let name_status_output = run_git(project_root, &["diff", "--name-status"])?;
    let branch = current_git_branch(project_root);
    let head = current_git_head(project_root);

    let mut changed_files = parse_name_status(&name_status_output);
    changed_files.extend(parse_untracked_from_status(&status_output));
    changed_files.sort_by(|left, right| left.path.cmp(&right.path));
    changed_files.dedup_by(|left, right| left.path == right.path);

    for file in &mut changed_files {
        file.change_types = classify_path(&file.path);
        file.suggested_docs = suggested_docs(&file.change_types);
    }

    let impacted_assets = build_impacted_assets(&changed_files);
    let summary = build_summary(&changed_files);

    Ok(ChangeAnalysis {
        schema_version: 1,
        project_root: project_root.display().to_string(),
        analyzed_at: Utc::now().to_rfc3339(),
        git_branch: branch,
        git_head: head,
        has_changes: !changed_files.is_empty(),
        changed_files,
        impacted_assets,
        summary,
    })
}

pub fn git_change_fingerprint(project_root: &Path) -> Result<String> {
    ensure_git_repository(project_root)?;

    let status_output = run_git(project_root, &["status", "--porcelain"])?;
    let name_status_output = run_git(project_root, &["diff", "--name-status"])?;

    Ok(format!("{}\n{}", status_output, name_status_output))
}

pub fn classify_path(path: &str) -> Vec<ChangeType> {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    let mut types = BTreeSet::new();

    if is_dependency_file(&lower) {
        types.insert(ChangeType::Dependency);
    }

    if is_environment_file(&lower, file_name) {
        types.insert(ChangeType::Environment);
    }

    if is_config_file(&lower, file_name) {
        types.insert(ChangeType::Config);
    }

    if is_api_file(&lower, file_name) {
        types.insert(ChangeType::Api);
    }

    if is_schema_file(&lower, file_name) {
        types.insert(ChangeType::Schema);
    }

    if is_documentation_file(&lower) {
        types.insert(ChangeType::Documentation);
    }

    if is_test_file(&lower, file_name) {
        types.insert(ChangeType::Test);
    }

    if is_architecture_change(&lower, file_name) {
        types.insert(ChangeType::Architecture);
    }

    if types.is_empty() && is_source_file(file_name) {
        types.insert(ChangeType::Source);
    }

    if types.is_empty() {
        types.insert(ChangeType::Unknown);
    }

    types.into_iter().collect()
}

fn ensure_git_repository(project_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .with_context(|| "无法执行 git 命令")?;

    if !output.status.success() {
        anyhow::bail!("当前目录不是 Git 工作区: {}", project_root.display());
    }

    Ok(())
}

fn run_git(project_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .output()
        .with_context(|| format!("无法执行 git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} 执行失败: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn current_git_branch(project_root: &Path) -> Option<String> {
    let output = run_git(project_root, &["branch", "--show-current"]).ok()?;
    let branch = output.trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn current_git_head(project_root: &Path) -> Option<String> {
    let output = run_git(project_root, &["rev-parse", "--short", "HEAD"]).ok()?;
    let head = output.trim().to_string();
    if head.is_empty() { None } else { Some(head) }
}

fn parse_name_status(output: &str) -> Vec<ChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let raw_status = parts.next()?;
            let status = parse_git_status(raw_status);
            let path = if raw_status.starts_with('R') || raw_status.starts_with('C') {
                parts.nth(1)?
            } else {
                parts.next()?
            };

            Some(ChangedFile {
                path: normalize_git_path(path),
                status,
                change_types: Vec::new(),
                suggested_docs: Vec::new(),
            })
        })
        .collect()
}

fn parse_untracked_from_status(output: &str) -> Vec<ChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            if !line.starts_with("?? ") {
                return None;
            }

            let path = line.trim_start_matches("?? ").trim();
            Some(ChangedFile {
                path: normalize_git_path(path),
                status: GitFileStatus::Untracked,
                change_types: Vec::new(),
                suggested_docs: Vec::new(),
            })
        })
        .collect()
}

fn parse_git_status(raw_status: &str) -> GitFileStatus {
    match raw_status.chars().next() {
        Some('A') => GitFileStatus::Added,
        Some('M') => GitFileStatus::Modified,
        Some('D') => GitFileStatus::Deleted,
        Some('R') => GitFileStatus::Renamed,
        Some('C') => GitFileStatus::Copied,
        Some('T') => GitFileStatus::TypeChanged,
        Some('U') => GitFileStatus::Unmerged,
        _ => GitFileStatus::Unknown,
    }
}

fn normalize_git_path(path: &str) -> String {
    path.trim().replace('\\', "/")
}

fn suggested_docs(types: &[ChangeType]) -> Vec<String> {
    let mut docs = BTreeSet::new();

    for change_type in types {
        match change_type {
            ChangeType::Api => {
                docs.insert("docs/api.md".to_string());
            }
            ChangeType::Schema => {
                docs.insert("docs/schema.md".to_string());
            }
            ChangeType::Dependency => {
                docs.insert("docs/dependencies.md".to_string());
            }
            ChangeType::Config | ChangeType::Environment => {
                docs.insert("docs/environment.md".to_string());
            }
            ChangeType::Architecture => {
                docs.insert("docs/architecture.md".to_string());
            }
            ChangeType::Documentation => {
                docs.insert("docs/changelog.md".to_string());
            }
            ChangeType::Source | ChangeType::Test | ChangeType::Unknown => {}
        }
    }

    docs.into_iter().collect()
}

fn build_impacted_assets(files: &[ChangedFile]) -> Vec<ImpactedAsset> {
    let mut assets = Vec::new();
    let mapping = [
        (ChangeType::Api, "docs/api.md", "接口定义发生变化"),
        (
            ChangeType::Schema,
            "docs/schema.md",
            "数据模型或数据库结构发生变化",
        ),
        (
            ChangeType::Dependency,
            "docs/dependencies.md",
            "依赖清单或锁文件发生变化",
        ),
        (
            ChangeType::Config,
            "docs/environment.md",
            "配置文件发生变化",
        ),
        (
            ChangeType::Environment,
            "docs/environment.md",
            "环境变量模板发生变化",
        ),
        (
            ChangeType::Architecture,
            "docs/architecture.md",
            "模块结构或架构入口发生变化",
        ),
        (
            ChangeType::Documentation,
            "docs/changelog.md",
            "项目文档发生变化，建议同步变更摘要",
        ),
    ];

    for (change_type, asset, reason) in mapping {
        let related_files = files
            .iter()
            .filter(|file| file.change_types.contains(&change_type))
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();

        if !related_files.is_empty() {
            assets.push(ImpactedAsset {
                asset: asset.to_string(),
                reason: reason.to_string(),
                related_files,
            });
        }
    }

    assets
}

fn build_summary(files: &[ChangedFile]) -> ChangeSummary {
    ChangeSummary {
        total_files: files.len(),
        api_changes: count_type(files, &ChangeType::Api),
        schema_changes: count_type(files, &ChangeType::Schema),
        dependency_changes: count_type(files, &ChangeType::Dependency),
        config_changes: count_type(files, &ChangeType::Config),
        environment_changes: count_type(files, &ChangeType::Environment),
        documentation_changes: count_type(files, &ChangeType::Documentation),
        architecture_changes: count_type(files, &ChangeType::Architecture),
    }
}

fn count_type(files: &[ChangedFile], change_type: &ChangeType) -> usize {
    files
        .iter()
        .filter(|file| file.change_types.contains(change_type))
        .count()
}

fn is_dependency_file(lower: &str) -> bool {
    matches!(
        lower,
        "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "requirements.txt"
            | "pyproject.toml"
            | "poetry.lock"
            | "go.mod"
            | "go.sum"
            | "cargo.toml"
            | "cargo.lock"
    ) || lower.ends_with("/package.json")
        || lower.ends_with("/cargo.toml")
}

fn is_environment_file(lower: &str, file_name: &str) -> bool {
    matches!(lower, ".env.example" | ".env.sample" | ".env.template")
        || file_name == ".env.example"
        || file_name == ".env.sample"
        || file_name == ".env.template"
}

fn is_config_file(lower: &str, file_name: &str) -> bool {
    file_name.starts_with("config.")
        || matches!(
            file_name,
            "application.yml"
                | "application.yaml"
                | "application.properties"
                | "tsconfig.json"
                | "vite.config.ts"
                | "vite.config.js"
                | "next.config.js"
                | "next.config.ts"
                | "tauri.conf.json"
        )
        || lower.starts_with("config/")
}

fn is_api_file(lower: &str, file_name: &str) -> bool {
    lower.contains("/controller/")
        || lower.contains("/controllers/")
        || lower.contains("/route/")
        || lower.contains("/routes/")
        || lower.contains("/api/")
        || file_name.contains("controller")
        || file_name.contains("route")
        || file_name == "openapi.yaml"
        || file_name == "openapi.yml"
        || file_name == "swagger.yaml"
        || file_name == "swagger.yml"
}

fn is_schema_file(lower: &str, file_name: &str) -> bool {
    lower.contains("/model/")
        || lower.contains("/models/")
        || lower.contains("/entity/")
        || lower.contains("/entities/")
        || lower.contains("/migration/")
        || lower.contains("/migrations/")
        || lower.contains("/schema/")
        || lower.ends_with(".sql")
        || file_name.contains("model")
        || file_name.contains("entity")
}

fn is_documentation_file(lower: &str) -> bool {
    lower == "readme.md" || lower.starts_with("docs/") || lower.starts_with("wiki/")
}

fn is_test_file(lower: &str, file_name: &str) -> bool {
    lower.contains("/test/")
        || lower.contains("/tests/")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with("test.java")
}

fn is_architecture_change(lower: &str, file_name: &str) -> bool {
    matches!(file_name, "mod.rs" | "lib.rs" | "main.rs")
        || lower.starts_with("crates/")
        || lower.starts_with("apps/")
        || lower.starts_with("packages/")
        || lower.starts_with("src/modules/")
}

fn is_source_file(file_name: &str) -> bool {
    file_name.ends_with(".rs")
        || file_name.ends_with(".ts")
        || file_name.ends_with(".tsx")
        || file_name.ends_with(".js")
        || file_name.ends_with(".jsx")
        || file_name.ends_with(".java")
        || file_name.ends_with(".kt")
        || file_name.ends_with(".py")
        || file_name.ends_with(".go")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_dependency_and_config_files() {
        assert_eq!(classify_path("package.json"), vec![ChangeType::Dependency]);
        assert_eq!(classify_path(".env.example"), vec![ChangeType::Environment]);
        assert!(classify_path("vite.config.ts").contains(&ChangeType::Config));
    }

    #[test]
    fn classify_api_schema_and_docs() {
        assert!(classify_path("src/controllers/user_controller.ts").contains(&ChangeType::Api));
        assert!(classify_path("src/models/user.ts").contains(&ChangeType::Schema));
        assert!(classify_path("migrations/001_create_user.sql").contains(&ChangeType::Schema));
        assert!(classify_path("docs/api.md").contains(&ChangeType::Documentation));
    }

    #[test]
    fn suggested_docs_are_deduped() {
        let docs = suggested_docs(&[ChangeType::Config, ChangeType::Environment]);
        assert_eq!(docs, vec!["docs/environment.md"]);
    }

    #[test]
    fn parse_untracked_status() {
        let files = parse_untracked_from_status("?? docs/new.md\n M src/lib.rs\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "docs/new.md");
        assert_eq!(files[0].status, GitFileStatus::Untracked);
    }
}
