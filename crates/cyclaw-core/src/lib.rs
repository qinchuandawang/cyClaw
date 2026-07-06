use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_change_radar::{ChangeAnalysis, analyze_git_changes, git_change_fingerprint};
use cyclaw_docs::{DocumentPatch, create_document_patch, mark_applied, parse_patch, render_patch};
use cyclaw_knowledge::{
    KnowledgeCandidate, KnowledgeStatus, candidates_from_change_analysis, parse_jsonl, render_jsonl,
};
use cyclaw_retrieval::{IndexSummary, SearchResult, build_index, search_index};
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
pub struct WatchOptions {
    pub project_root: PathBuf,
    pub interval_seconds: u64,
    pub once: bool,
}

impl WatchOptions {
    pub fn new(project_root: PathBuf, interval_seconds: u64, once: bool) -> Self {
        Self {
            project_root,
            interval_seconds,
            once,
        }
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

#[derive(Debug, Clone)]
pub struct WatchTick {
    pub changed: bool,
    pub diff_result: Option<DiffResult>,
    pub inbox_result: Option<InboxGenerateResult>,
}

#[derive(Debug, Clone)]
pub struct InboxGenerateOptions {
    pub project_root: PathBuf,
    pub analysis_path: Option<PathBuf>,
}

impl InboxGenerateOptions {
    pub fn new(project_root: PathBuf, analysis_path: Option<PathBuf>) -> Self {
        Self {
            project_root,
            analysis_path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InboxGenerateResult {
    pub inbox_path: PathBuf,
    pub source_analysis_path: PathBuf,
    pub generated: Vec<KnowledgeCandidate>,
    pub total_pending: usize,
}

#[derive(Debug, Clone)]
pub struct InboxListResult {
    pub inbox_path: PathBuf,
    pub candidates: Vec<KnowledgeCandidate>,
}

#[derive(Debug, Clone)]
pub struct InboxUpdateResult {
    pub inbox_path: PathBuf,
    pub candidate: KnowledgeCandidate,
}

#[derive(Debug, Clone)]
pub struct DraftOptions {
    pub project_root: PathBuf,
    pub candidate_id: Option<String>,
    pub include_pending: bool,
}

impl DraftOptions {
    pub fn new(project_root: PathBuf, candidate_id: Option<String>, include_pending: bool) -> Self {
        Self {
            project_root,
            candidate_id,
            include_pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DraftResult {
    pub patches_dir: PathBuf,
    pub patches: Vec<DocumentPatch>,
}

#[derive(Debug, Clone)]
pub struct ApplyPatchResult {
    pub patch_path: PathBuf,
    pub target_doc_path: PathBuf,
    pub patch: DocumentPatch,
}

#[derive(Debug, Clone)]
pub struct ProjectStatus {
    pub project_root: PathBuf,
    pub initialized: bool,
    pub config_exists: bool,
    pub project_profile_exists: bool,
    pub project_doc_exists: bool,
    pub latest_run: Option<PathBuf>,
    pub inbox_exists: bool,
    pub inbox_total: usize,
    pub inbox_pending: usize,
    pub draft_total: usize,
    pub draft_pending: usize,
    pub index_exists: bool,
    pub git_has_changes: bool,
    pub suggested_next_steps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub project_root: PathBuf,
    pub query: String,
    pub limit: usize,
}

impl SearchOptions {
    pub fn new(project_root: PathBuf, query: String, limit: usize) -> Self {
        Self {
            project_root,
            query,
            limit,
        }
    }
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

pub fn generate_inbox(options: InboxGenerateOptions) -> Result<InboxGenerateResult> {
    let project_root = options.project_root;
    ensure_directory(&project_root)?;
    ensure_cyclaw_dir(&project_root)?;

    let analysis_path = match options.analysis_path {
        Some(path) => path,
        None => latest_change_analysis_path(&project_root)?,
    };
    let analysis = read_change_analysis(&analysis_path)?;
    let generated = candidates_from_change_analysis(
        &analysis,
        &relative_or_display(&project_root, &analysis_path),
    );

    let inbox_path = inbox_path(&project_root);
    let mut existing = read_inbox_candidates(&project_root)?;
    append_new_candidates(&mut existing, &generated);
    write_inbox_candidates(&inbox_path, &existing)?;

    let total_pending = existing
        .iter()
        .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
        .count();

    Ok(InboxGenerateResult {
        inbox_path,
        source_analysis_path: analysis_path,
        generated,
        total_pending,
    })
}

pub fn list_inbox(project_root: PathBuf) -> Result<InboxListResult> {
    ensure_directory(&project_root)?;
    let inbox_path = inbox_path(&project_root);
    let candidates = read_inbox_candidates(&project_root)?;

    Ok(InboxListResult {
        inbox_path,
        candidates,
    })
}

pub fn update_inbox_status(
    project_root: PathBuf,
    candidate_id: &str,
    status: KnowledgeStatus,
) -> Result<InboxUpdateResult> {
    ensure_directory(&project_root)?;

    let inbox_path = inbox_path(&project_root);
    let mut candidates = read_inbox_candidates(&project_root)?;
    let Some(candidate) = candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
    else {
        anyhow::bail!("未找到候选知识: {}", candidate_id);
    };

    candidate.status = status;
    candidate.updated_at = Utc::now().to_rfc3339();
    let updated = candidate.clone();
    write_inbox_candidates(&inbox_path, &candidates)?;

    Ok(InboxUpdateResult {
        inbox_path,
        candidate: updated,
    })
}

pub fn generate_document_drafts(options: DraftOptions) -> Result<DraftResult> {
    let project_root = options.project_root;
    ensure_directory(&project_root)?;

    let patches_dir = doc_patches_dir(&project_root);
    fs::create_dir_all(&patches_dir)
        .with_context(|| format!("无法创建文档草稿目录: {}", patches_dir.display()))?;

    let candidates = read_inbox_candidates(&project_root)?;
    let selected = candidates
        .iter()
        .filter(|candidate| {
            options
                .candidate_id
                .as_ref()
                .map(|id| candidate.id == *id)
                .unwrap_or(true)
        })
        .filter(|candidate| {
            candidate.status == KnowledgeStatus::Accepted
                || (options.include_pending && candidate.status == KnowledgeStatus::Pending)
        })
        .cloned()
        .collect::<Vec<_>>();

    if selected.is_empty() {
        anyhow::bail!("没有可生成文档草稿的候选知识");
    }

    let mut patches = Vec::new();
    for candidate in selected {
        let target_doc_path = safe_target_doc_path(&project_root, &candidate.recommended_doc)?;
        let original_content = if target_doc_path.exists() {
            fs::read_to_string(&target_doc_path)
                .with_context(|| format!("无法读取目标文档: {}", target_doc_path.display()))?
        } else {
            render_new_doc_template(&candidate.recommended_doc)
        };
        let patch = create_document_patch(&candidate, &original_content);
        let patch_path = patches_dir.join(format!("{}.json", patch.id));
        fs::write(&patch_path, render_patch(&patch)?)
            .with_context(|| format!("无法写入文档草稿: {}", patch_path.display()))?;
        patches.push(patch);
    }

    Ok(DraftResult {
        patches_dir,
        patches,
    })
}

pub fn list_document_patches(project_root: PathBuf) -> Result<Vec<DocumentPatch>> {
    ensure_directory(&project_root)?;
    let patches_dir = doc_patches_dir(&project_root);
    if !patches_dir.exists() {
        return Ok(Vec::new());
    }

    let mut patches = Vec::new();
    for entry in fs::read_dir(&patches_dir)
        .with_context(|| format!("无法读取文档草稿目录: {}", patches_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("无法读取文档草稿: {}", path.display()))?;
        patches.push(parse_patch(&content)?);
    }

    patches.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(patches)
}

pub fn apply_document_patch(project_root: PathBuf, patch_id: &str) -> Result<ApplyPatchResult> {
    ensure_directory(&project_root)?;

    let patch_path = doc_patches_dir(&project_root).join(format!("{}.json", patch_id));
    let content = fs::read_to_string(&patch_path)
        .with_context(|| format!("无法读取文档草稿: {}", patch_path.display()))?;
    let mut patch = parse_patch(&content)?;
    let target_doc_path = safe_target_doc_path(&project_root, &patch.target_doc)?;

    if let Some(parent) = target_doc_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建文档目录: {}", parent.display()))?;
    }

    fs::write(&target_doc_path, &patch.proposed_content)
        .with_context(|| format!("无法写入目标文档: {}", target_doc_path.display()))?;
    mark_applied(&mut patch);
    fs::write(&patch_path, render_patch(&patch)?)
        .with_context(|| format!("无法更新文档草稿状态: {}", patch_path.display()))?;

    Ok(ApplyPatchResult {
        patch_path,
        target_doc_path,
        patch,
    })
}

pub fn project_status(project_root: PathBuf) -> Result<ProjectStatus> {
    ensure_directory(&project_root)?;

    let cyclaw_dir = project_root.join(CYCLE_DIR);
    let config_path = cyclaw_dir.join("config.yaml");
    let profile_path = cyclaw_dir.join("project-profile.json");
    let project_doc_path = cyclaw_dir.join("project.md");
    let latest_run = latest_change_analysis_path(&project_root).ok();
    let inbox = read_inbox_candidates(&project_root).unwrap_or_default();
    let drafts = list_document_patches(project_root.clone()).unwrap_or_default();
    let index = index_path(&project_root);
    let git_has_changes = current_change_fingerprint(&project_root)
        .map(|fingerprint| !fingerprint.trim().is_empty())
        .unwrap_or(false);

    let inbox_pending = inbox
        .iter()
        .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
        .count();
    let draft_pending = drafts
        .iter()
        .filter(|patch| patch.status == cyclaw_docs::DocumentPatchStatus::Pending)
        .count();

    let mut status = ProjectStatus {
        project_root,
        initialized: cyclaw_dir.exists(),
        config_exists: config_path.exists(),
        project_profile_exists: profile_path.exists(),
        project_doc_exists: project_doc_path.exists(),
        latest_run,
        inbox_exists: inbox_path_exists(&cyclaw_dir),
        inbox_total: inbox.len(),
        inbox_pending,
        draft_total: drafts.len(),
        draft_pending,
        index_exists: index.exists(),
        git_has_changes,
        suggested_next_steps: Vec::new(),
    };
    status.suggested_next_steps = suggested_next_steps(&status);

    Ok(status)
}

pub fn index_project(project_root: PathBuf) -> Result<IndexSummary> {
    ensure_directory(&project_root)?;
    let index_path = index_path(&project_root);
    build_index(&project_root, &index_path)
}

pub fn search_project(options: SearchOptions) -> Result<Vec<SearchResult>> {
    ensure_directory(&options.project_root)?;
    let index_path = index_path(&options.project_root);
    if !index_path.exists() {
        index_project(options.project_root.clone())?;
    }
    search_index(&index_path, &options.query, options.limit)
}

pub fn current_change_fingerprint(project_root: &Path) -> Result<String> {
    git_change_fingerprint(project_root)
}

pub fn watch_project_once(
    project_root: &Path,
    last_fingerprint: Option<&str>,
) -> Result<WatchTick> {
    ensure_directory(project_root)?;

    let fingerprint = current_change_fingerprint(project_root)?;
    if last_fingerprint == Some(fingerprint.as_str()) {
        return Ok(WatchTick {
            changed: false,
            diff_result: None,
            inbox_result: None,
        });
    }

    let diff_result = analyze_project_diff(DiffOptions::new(project_root.to_path_buf()))?;
    let inbox_result = generate_inbox(InboxGenerateOptions::new(
        project_root.to_path_buf(),
        Some(diff_result.analysis_path.clone()),
    ))?;
    Ok(WatchTick {
        changed: true,
        diff_result: Some(diff_result),
        inbox_result: Some(inbox_result),
    })
}

fn ensure_cyclaw_dir(project_root: &Path) -> Result<PathBuf> {
    let cyclaw_dir = project_root.join(CYCLE_DIR);
    fs::create_dir_all(&cyclaw_dir)
        .with_context(|| format!("无法创建 cyClaw 目录: {}", cyclaw_dir.display()))?;
    Ok(cyclaw_dir)
}

fn inbox_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("knowledge-inbox.jsonl")
}

fn inbox_path_exists(cyclaw_dir: &Path) -> bool {
    cyclaw_dir.join("knowledge-inbox.jsonl").exists()
}

fn doc_patches_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("doc-patches")
}

fn index_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("index.sqlite")
}

fn suggested_next_steps(status: &ProjectStatus) -> Vec<String> {
    let mut steps = Vec::new();

    if !status.initialized || !status.config_exists {
        steps.push("运行 `cyclaw init` 初始化项目知识目录".to_string());
        return steps;
    }

    if !status.project_profile_exists || !status.project_doc_exists {
        steps.push("运行 `cyclaw scan` 刷新项目画像".to_string());
    }

    if status.git_has_changes {
        steps.push("运行 `cyclaw watch --once --interval 0` 自动分析当前变更".to_string());
    }

    if status.inbox_pending > 0 {
        steps.push("运行 `cyclaw inbox list --pending` 查看待处理候选知识".to_string());
        steps.push("运行 `cyclaw inbox accept <id>` 接受重要候选知识".to_string());
    }

    if status.draft_pending > 0 {
        steps.push("运行 `cyclaw draft list` 查看文档草稿".to_string());
        steps.push("运行 `cyclaw draft apply <id>` 应用确认后的文档草稿".to_string());
    }

    if !status.index_exists {
        steps.push("运行 `cyclaw index` 构建本地知识索引".to_string());
    }

    if steps.is_empty() {
        steps.push("当前 CLI/Core 主链路状态正常，可继续开发或启动 watch".to_string());
    }

    steps
}

fn safe_target_doc_path(project_root: &Path, target_doc: &str) -> Result<PathBuf> {
    let normalized = target_doc.replace('\\', "/");
    let allowed = normalized.starts_with("docs/") || normalized.starts_with(".cyclaw/");
    if !allowed || normalized.contains("..") {
        anyhow::bail!("不允许写入目标文档路径: {}", target_doc);
    }

    Ok(project_root.join(normalized))
}

fn render_new_doc_template(target_doc: &str) -> String {
    let title = target_doc
        .trim_end_matches(".md")
        .rsplit('/')
        .next()
        .unwrap_or("document")
        .replace('-', " ");
    format!("# {}\n", title)
}

fn latest_change_analysis_path(project_root: &Path) -> Result<PathBuf> {
    let runs_dir = project_root.join(CYCLE_DIR).join("runs");
    let mut candidates = Vec::new();

    if runs_dir.exists() {
        for entry in fs::read_dir(&runs_dir)
            .with_context(|| format!("无法读取 runs 目录: {}", runs_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path().join("change-analysis.json");
            if path.exists() {
                candidates.push(path);
            }
        }
    }

    candidates.sort();
    candidates
        .pop()
        .with_context(|| "未找到 change-analysis.json，请先运行 cyclaw diff")
}

fn read_change_analysis(path: &Path) -> Result<ChangeAnalysis> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("无法读取变更分析: {}", path.display()))?;
    Ok(serde_json::from_str(&content)?)
}

fn read_inbox_candidates(project_root: &Path) -> Result<Vec<KnowledgeCandidate>> {
    let path = inbox_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取知识收件箱: {}", path.display()))?;
    parse_jsonl(&content)
}

fn write_inbox_candidates(path: &Path, candidates: &[KnowledgeCandidate]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建知识收件箱目录: {}", parent.display()))?;
    }

    let content = render_jsonl(candidates)?;
    fs::write(path, content).with_context(|| format!("无法写入知识收件箱: {}", path.display()))
}

fn append_new_candidates(existing: &mut Vec<KnowledgeCandidate>, generated: &[KnowledgeCandidate]) {
    let existing_ids = existing
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    for candidate in generated {
        if !existing_ids.contains(&candidate.id) {
            existing.push(candidate.clone());
        }
    }
}

fn relative_or_display(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
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

    #[test]
    fn generate_and_update_inbox_candidates() {
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

        let diff_result =
            analyze_project_diff(DiffOptions::new(temp.path().to_path_buf())).unwrap();
        let inbox_result = generate_inbox(InboxGenerateOptions::new(
            temp.path().to_path_buf(),
            Some(diff_result.analysis_path),
        ))
        .unwrap();

        assert!(inbox_result.inbox_path.exists());
        assert_eq!(inbox_result.generated.len(), 1);
        assert_eq!(inbox_result.total_pending, 1);

        let candidate_id = inbox_result.generated[0].id.clone();
        let update_result = update_inbox_status(
            temp.path().to_path_buf(),
            &candidate_id,
            KnowledgeStatus::Accepted,
        )
        .unwrap();
        assert_eq!(update_result.candidate.status, KnowledgeStatus::Accepted);

        let list_result = list_inbox(temp.path().to_path_buf()).unwrap();
        assert_eq!(list_result.candidates.len(), 1);
        assert_eq!(list_result.candidates[0].status, KnowledgeStatus::Accepted);
    }

    #[test]
    fn generate_and_apply_document_patch() {
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

        let diff_result =
            analyze_project_diff(DiffOptions::new(temp.path().to_path_buf())).unwrap();
        let inbox_result = generate_inbox(InboxGenerateOptions::new(
            temp.path().to_path_buf(),
            Some(diff_result.analysis_path),
        ))
        .unwrap();
        let candidate_id = inbox_result.generated[0].id.clone();
        update_inbox_status(
            temp.path().to_path_buf(),
            &candidate_id,
            KnowledgeStatus::Accepted,
        )
        .unwrap();

        let draft_result = generate_document_drafts(DraftOptions::new(
            temp.path().to_path_buf(),
            Some(candidate_id),
            false,
        ))
        .unwrap();
        assert_eq!(draft_result.patches.len(), 1);

        let patch_id = draft_result.patches[0].id.clone();
        let apply_result = apply_document_patch(temp.path().to_path_buf(), &patch_id).unwrap();
        assert!(apply_result.target_doc_path.exists());

        let content = fs::read_to_string(apply_result.target_doc_path).unwrap();
        assert!(content.contains("来源候选"));
        assert!(content.contains("package.json"));
    }

    #[test]
    fn index_and_search_project_knowledge() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("docs")).unwrap();
        fs::write(
            temp.path().join("docs").join("api.md"),
            "# API\n\n支付回调必须校验签名。",
        )
        .unwrap();

        let summary = index_project(temp.path().to_path_buf()).unwrap();
        let results = search_project(SearchOptions::new(
            temp.path().to_path_buf(),
            "支付".to_string(),
            10,
        ))
        .unwrap();

        assert!(summary.index_path.exists());
        assert_eq!(summary.document_count, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "docs/api.md");
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
