use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_change_radar::{
    ChangeAnalysis, GitChangeSnapshot, analyze_git_changes, changed_paths_since,
    filter_change_analysis, git_change_fingerprint, git_change_snapshot,
};
use cyclaw_docs::{
    DocumentPatch, DocumentPatchOptions, KnowledgeOperation, create_document_patch_with_options,
    mark_applied, mark_reverted, parse_patch, render_patch,
};
use cyclaw_events::{AgentEventType, append_event, new_event};
use cyclaw_knowledge::{
    KnowledgeCandidate, KnowledgeStatus, candidates_from_change_analysis, parse_jsonl, render_jsonl,
};
pub use cyclaw_memory::{
    BeginTaskOptions, FactContext, FailedApproach, ProjectFact, ReconciliationReport,
    TaskCheckpoint, TaskDecision, TaskRecord,
};
use cyclaw_memory::{
    begin_task as memory_begin_task, checkpoint_task as memory_checkpoint_task,
    close_task as memory_close_task, compile_fact_context, get_active_task as memory_active_task,
    get_task as memory_get_task, latest_reconciliation as memory_latest_reconciliation,
    list_facts as memory_list_facts, list_tasks as memory_list_tasks,
    reconcile_knowledge as memory_reconcile_knowledge, record_decision as memory_record_decision,
    record_failed_approach as memory_record_failed_approach,
};
use cyclaw_policy::{PermissionLevel, acquire_lock, check_write_path, load_or_default};
use cyclaw_retrieval::{IndexSummary, SearchResult, build_index, search_index};
use cyclaw_scanner::{ProjectProfile, scan_project_profile};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
    pub changed_paths: Option<BTreeSet<String>>,
}

impl DiffOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            changed_paths: None,
        }
    }

    pub fn incremental(mut self, changed_paths: BTreeSet<String>) -> Self {
        self.changed_paths = Some(changed_paths);
        self
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
    pub auto_applied_patches: usize,
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
    pub added: Vec<KnowledgeCandidate>,
    pub existing_count: usize,
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
    pub min_confidence: Option<u8>,
    pub require_model_review: bool,
    pub operation: Option<KnowledgeOperation>,
    pub selector: Option<String>,
    pub source_selectors: Vec<String>,
    pub replacement_content: Option<String>,
    pub delete_target_document: bool,
}

impl DraftOptions {
    pub fn new(project_root: PathBuf, candidate_id: Option<String>, include_pending: bool) -> Self {
        Self {
            project_root,
            candidate_id,
            include_pending,
            min_confidence: None,
            require_model_review: false,
            operation: None,
            selector: None,
            source_selectors: Vec::new(),
            replacement_content: None,
            delete_target_document: false,
        }
    }

    pub fn with_confidence(mut self, min_confidence: u8, require_model_review: bool) -> Self {
        self.min_confidence = Some(min_confidence.min(100));
        self.require_model_review = require_model_review;
        self
    }

    pub fn with_operation(
        mut self,
        operation: KnowledgeOperation,
        selector: Option<String>,
        source_selectors: Vec<String>,
        replacement_content: Option<String>,
        delete_target_document: bool,
    ) -> Self {
        self.operation = Some(operation);
        self.selector = selector;
        self.source_selectors = source_selectors;
        self.replacement_content = replacement_content;
        self.delete_target_document = delete_target_document;
        self
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContextPack {
    pub task: TaskRecord,
    pub query: String,
    pub facts: FactContext,
    pub documents: Vec<SearchResult>,
    pub pending_candidates: Vec<KnowledgeCandidate>,
    pub related_files: Vec<String>,
    pub estimated_tokens: usize,
    pub budget_tokens: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseTaskResult {
    pub task: TaskRecord,
    pub reconciliation: Option<ReconciliationReport>,
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
    let _lock = acquire_lock(&project_root, "project", Duration::from_secs(5))?;

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
    record_event(
        &project_root,
        AgentEventType::ProjectScanned,
        "cyclaw-core",
        "项目扫描完成",
        serde_json::json!({ "languages": profile.languages.clone(), "frameworks": profile.frameworks.clone() }),
    )?;

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
    let _lock = acquire_lock(&project_root, "project", Duration::from_secs(5))?;

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
    let _lock = acquire_lock(&project_root, "diff", Duration::from_secs(5))?;

    let cyclaw_dir = project_root.join(CYCLE_DIR);
    let runs_dir = cyclaw_dir.join("runs");
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("无法创建 runs 目录: {}", runs_dir.display()))?;

    let analysis = analyze_git_changes(&project_root)?;
    let analysis = match options.changed_paths.as_ref() {
        Some(changed_paths) => filter_change_analysis(analysis, changed_paths),
        None => analysis,
    };
    let run_id = format!("diff-{}", Utc::now().format("%Y%m%d%H%M%S"));
    let run_dir = runs_dir.join(&run_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("无法创建 run 目录: {}", run_dir.display()))?;

    let analysis_path = run_dir.join("change-analysis.json");
    let json = serde_json::to_string_pretty(&analysis)?;
    fs::write(&analysis_path, json)
        .with_context(|| format!("无法写入变更分析: {}", analysis_path.display()))?;
    record_event(
        &project_root,
        AgentEventType::GitChanged,
        "cyclaw-core",
        "完成 Git 变更分析",
        serde_json::json!({ "run_id": run_id.clone(), "files": analysis.summary.total_files }),
    )?;

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
    let _lock = acquire_lock(&project_root, "inbox", Duration::from_secs(5))?;
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
    let added = append_new_candidates(&mut existing, &generated);
    let existing_count = generated.len().saturating_sub(added.len());
    write_inbox_candidates(&inbox_path, &existing)?;

    let total_pending = existing
        .iter()
        .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
        .count();

    if !added.is_empty() {
        record_event(
            &project_root,
            AgentEventType::KnowledgeCandidateCreated,
            "cyclaw-core",
            "生成知识候选",
            serde_json::json!({
                "analyzed": generated.len(),
                "added": added.len(),
                "existing": existing_count,
                "source": relative_or_display(&project_root, &analysis_path)
            }),
        )?;
    }

    Ok(InboxGenerateResult {
        inbox_path,
        source_analysis_path: analysis_path,
        generated,
        added,
        existing_count,
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
    let _lock = acquire_lock(&project_root, "inbox", Duration::from_secs(5))?;

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
    record_event(
        &project_root,
        AgentEventType::KnowledgeCandidateUpdated,
        "cyclaw-core",
        "更新知识候选状态",
        serde_json::json!({ "candidate_id": candidate_id, "status": updated.status.clone() }),
    )?;

    Ok(InboxUpdateResult {
        inbox_path,
        candidate: updated,
    })
}

pub fn update_candidate_review(
    project_root: PathBuf,
    candidate_id: &str,
    confidence: u8,
    recommendation: String,
    rationale: String,
) -> Result<InboxUpdateResult> {
    ensure_directory(&project_root)?;
    let _lock = acquire_lock(&project_root, "inbox", Duration::from_secs(5))?;
    let inbox_path = inbox_path(&project_root);
    let mut candidates = read_inbox_candidates(&project_root)?;
    let candidate = candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
        .with_context(|| format!("未找到候选知识: {}", candidate_id))?;
    candidate.confidence = confidence.min(100);
    candidate.reviewed_by_model = true;
    candidate.model_recommendation = Some(recommendation);
    candidate.model_rationale = Some(rationale);
    candidate.updated_at = Utc::now().to_rfc3339();
    let updated = candidate.clone();
    write_inbox_candidates(&inbox_path, &candidates)?;
    record_event(
        &project_root,
        AgentEventType::KnowledgeCandidateUpdated,
        "cyclaw-core",
        "模型审查知识候选",
        serde_json::json!({ "candidate_id": candidate_id, "confidence": updated.confidence }),
    )?;
    Ok(InboxUpdateResult {
        inbox_path,
        candidate: updated,
    })
}

pub fn generate_document_drafts(options: DraftOptions) -> Result<DraftResult> {
    if options.operation.is_some() && options.candidate_id.is_none() {
        anyhow::bail!("显式知识操作必须通过 candidate_id 指定单个候选知识");
    }
    let project_root = options.project_root;
    ensure_directory(&project_root)?;
    let _lock = acquire_lock(&project_root, "docs", Duration::from_secs(5))?;

    let patches_dir = doc_patches_dir(&project_root);
    fs::create_dir_all(&patches_dir)
        .with_context(|| format!("无法创建文档草稿目录: {}", patches_dir.display()))?;

    let candidates = read_inbox_candidates(&project_root)?;
    let existing_candidate_ids = if options.operation.is_none() {
        list_document_patches(project_root.clone())?
            .into_iter()
            .map(|patch| patch.candidate_id)
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
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
        .filter(|candidate| !existing_candidate_ids.contains(&candidate.id))
        .filter(|candidate| {
            options
                .min_confidence
                .map(|minimum| candidate.confidence >= minimum)
                .unwrap_or(true)
        })
        .filter(|candidate| {
            !options.require_model_review
                || (candidate.reviewed_by_model
                    && candidate.model_recommendation.as_deref() != Some("ignore"))
        })
        .cloned()
        .collect::<Vec<_>>();

    if selected.is_empty() {
        anyhow::bail!("没有可生成文档草稿的候选知识");
    }

    let mut patches = Vec::new();
    for candidate in selected {
        let target_doc_path = safe_target_doc_path(&project_root, &candidate.recommended_doc)?;
        let target_existed = target_doc_path.exists();
        if options.delete_target_document && !target_existed {
            anyhow::bail!(
                "目标文档不存在，不能生成整文档删除草稿: {}",
                candidate.recommended_doc
            );
        }
        let original_content = if target_existed {
            fs::read_to_string(&target_doc_path)
                .with_context(|| format!("无法读取目标文档: {}", target_doc_path.display()))?
        } else {
            render_new_doc_template(&candidate.recommended_doc)
        };
        let patch = create_document_patch_with_options(
            &candidate,
            &original_content,
            DocumentPatchOptions {
                operation: options.operation.clone(),
                selector: options.selector.clone(),
                source_selectors: options.source_selectors.clone(),
                replacement_content: options.replacement_content.clone(),
                delete_target_document: options.delete_target_document,
                target_existed,
            },
        )?;
        let patch_path = patches_dir.join(format!("{}.json", patch.id));
        fs::write(&patch_path, render_patch(&patch)?)
            .with_context(|| format!("无法写入文档草稿: {}", patch_path.display()))?;
        record_event(
            &project_root,
            AgentEventType::DocumentPatchCreated,
            "cyclaw-core",
            "生成文档草稿",
            serde_json::json!({
                "patch_id": patch.id.clone(),
                "target_doc": patch.target_doc.clone(),
                "operation": patch.operation.as_str()
            }),
        )?;
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
    let _lock = acquire_lock(&project_root, "docs", Duration::from_secs(5))?;

    let patch_path = doc_patches_dir(&project_root).join(format!("{}.json", patch_id));
    let content = fs::read_to_string(&patch_path)
        .with_context(|| format!("无法读取文档草稿: {}", patch_path.display()))?;
    let mut patch = parse_patch(&content)?;
    let target_doc_path = safe_target_doc_path(&project_root, &patch.target_doc)?;
    let policy_check =
        check_write_path(&project_root, &patch.target_doc, PermissionLevel::DocsWrite)?;
    if !policy_check.allowed {
        anyhow::bail!("权限拒绝写入目标文档: {}", policy_check.reason);
    }
    let policy = load_or_default(&project_root)?;
    if !policy.permissions.allow_docs_apply {
        anyhow::bail!("权限拒绝应用文档草稿：请先运行 `cyclaw policy enable-docs-apply`");
    }

    let current_content =
        if target_doc_path.exists() {
            Some(fs::read_to_string(&target_doc_path).with_context(|| {
                format!("无法读取应用前目标文档: {}", target_doc_path.display())
            })?)
        } else {
            None
        };
    if patch.target_existed {
        if current_content.as_deref() != Some(patch.original_content.as_str()) {
            anyhow::bail!("目标文档在草稿生成后已变化，请重新生成草稿后再应用");
        }
    } else if current_content.is_some() {
        anyhow::bail!("目标文档在草稿生成后已被创建，请重新生成草稿后再应用");
    }

    if let Some(parent) = target_doc_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建文档目录: {}", parent.display()))?;
    }

    if patch.delete_target_document {
        if target_doc_path.exists() {
            fs::remove_file(&target_doc_path)
                .with_context(|| format!("无法删除目标文档: {}", target_doc_path.display()))?;
        }
    } else {
        fs::write(&target_doc_path, &patch.proposed_content)
            .with_context(|| format!("无法写入目标文档: {}", target_doc_path.display()))?;
    }
    mark_applied(&mut patch);
    fs::write(&patch_path, render_patch(&patch)?)
        .with_context(|| format!("无法更新文档草稿状态: {}", patch_path.display()))?;
    record_event(
        &project_root,
        AgentEventType::DocumentPatchApplied,
        "cyclaw-core",
        "应用文档草稿",
        serde_json::json!({
            "patch_id": patch.id.clone(),
            "target_doc": patch.target_doc.clone(),
            "operation": patch.operation.as_str()
        }),
    )?;

    Ok(ApplyPatchResult {
        patch_path,
        target_doc_path,
        patch,
    })
}

pub fn revert_document_patch(project_root: PathBuf, patch_id: &str) -> Result<ApplyPatchResult> {
    ensure_directory(&project_root)?;
    let _lock = acquire_lock(&project_root, "docs", Duration::from_secs(5))?;
    let patch_path = doc_patches_dir(&project_root).join(format!("{}.json", patch_id));
    let content = fs::read_to_string(&patch_path)
        .with_context(|| format!("无法读取文档草稿: {}", patch_path.display()))?;
    let mut patch = parse_patch(&content)?;
    let target_doc_path = safe_target_doc_path(&project_root, &patch.target_doc)?;
    let policy_check =
        check_write_path(&project_root, &patch.target_doc, PermissionLevel::DocsWrite)?;
    if !policy_check.allowed || !load_or_default(&project_root)?.permissions.allow_docs_apply {
        anyhow::bail!("权限拒绝撤销文档草稿");
    }
    if patch.delete_target_document {
        if target_doc_path.exists() {
            anyhow::bail!("被删除的目标文档已重新出现，拒绝覆盖，请人工检查");
        }
    } else {
        let current_content = fs::read_to_string(&target_doc_path)
            .with_context(|| format!("无法读取撤销前目标文档: {}", target_doc_path.display()))?;
        if current_content != patch.proposed_content {
            anyhow::bail!("目标文档在草稿应用后已变化，拒绝撤销以避免覆盖新内容");
        }
    }

    if patch.target_existed {
        if let Some(parent) = target_doc_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target_doc_path, &patch.original_content)
            .with_context(|| format!("无法恢复目标文档: {}", target_doc_path.display()))?;
    } else if target_doc_path.exists() {
        fs::remove_file(&target_doc_path)
            .with_context(|| format!("无法移除新建文档: {}", target_doc_path.display()))?;
    }
    mark_reverted(&mut patch);
    fs::write(&patch_path, render_patch(&patch)?)?;
    record_event(
        &project_root,
        AgentEventType::DocumentPatchApplied,
        "cyclaw-core",
        "撤销文档草稿",
        serde_json::json!({
            "patch_id": patch.id.clone(),
            "target_doc": patch.target_doc.clone(),
            "operation": patch.operation.as_str(),
            "reverted": true
        }),
    )?;
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
    let _lock = acquire_lock(&project_root, "index", Duration::from_secs(5))?;
    let index_path = index_path(&project_root);
    let result = build_index(&project_root, &index_path)?;
    record_event(
        &project_root,
        AgentEventType::IndexUpdated,
        "cyclaw-core",
        "本地知识索引完成",
        serde_json::json!({ "document_count": result.document_count }),
    )?;
    Ok(result)
}

pub fn search_project(options: SearchOptions) -> Result<Vec<SearchResult>> {
    ensure_directory(&options.project_root)?;
    let index_path = index_path(&options.project_root);
    if !index_path.exists() {
        index_project(options.project_root.clone())?;
    }
    search_index(&index_path, &options.query, options.limit)
}

pub fn begin_task(mut options: BeginTaskOptions) -> Result<TaskRecord> {
    ensure_directory(&options.project_root)?;
    if options.git_head.is_none() {
        options.git_head = current_git_head(&options.project_root);
    }
    memory_begin_task(options)
}

pub fn get_active_task(project_root: &Path) -> Result<Option<TaskRecord>> {
    memory_active_task(project_root)
}

pub fn get_task(project_root: &Path, task_id: &str) -> Result<TaskRecord> {
    memory_get_task(project_root, task_id)
}

pub fn list_tasks(project_root: &Path, limit: usize) -> Result<Vec<TaskRecord>> {
    memory_list_tasks(project_root, limit)
}

pub fn list_project_facts(project_root: &Path) -> Result<Vec<ProjectFact>> {
    memory_list_facts(project_root)
}

pub fn get_latest_reconciliation(project_root: &Path) -> Result<Option<ReconciliationReport>> {
    memory_latest_reconciliation(project_root)
}

pub fn get_task_context(
    project_root: &Path,
    task_id: Option<&str>,
    query: Option<&str>,
    budget_tokens: Option<usize>,
    limit: usize,
) -> Result<TaskContextPack> {
    let task = match task_id {
        Some(id) => memory_get_task(project_root, id)?,
        None => {
            memory_active_task(project_root)?.context("当前没有活动任务，请先调用 begin_task")?
        }
    };
    let query = query
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{} {}", task.title, task.objective));
    let budget = budget_tokens
        .unwrap_or(task.context_budget_tokens)
        .clamp(256, 16_000);
    let fact_budget = (budget * 60 / 100).max(128);
    let facts = compile_fact_context(project_root, &query, fact_budget, limit)?;
    let mut estimated_tokens = facts.estimated_tokens;
    let mut truncated = facts.truncated;
    let mut documents = search_project(SearchOptions::new(
        project_root.to_path_buf(),
        query.clone(),
        limit.clamp(1, 20),
    ))
    .unwrap_or_default();
    documents.retain(|document| {
        let tokens = estimate_context_tokens(&document.title)
            + estimate_context_tokens(&document.snippet)
            + estimate_context_tokens(&document.path);
        if estimated_tokens + tokens > budget {
            truncated = true;
            false
        } else {
            estimated_tokens += tokens;
            true
        }
    });

    let mut candidates = list_inbox(project_root.to_path_buf())?
        .candidates
        .into_iter()
        .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|item| std::cmp::Reverse(item.confidence));
    let related = task.related_files.iter().collect::<HashSet<_>>();
    candidates.sort_by_key(|candidate| {
        let directly_related = candidate
            .related_files
            .iter()
            .any(|path| related.contains(path));
        (!directly_related, std::cmp::Reverse(candidate.confidence))
    });
    candidates.truncate(limit.clamp(1, 20));
    candidates.retain(|candidate| {
        let tokens = estimate_context_tokens(&candidate.summary)
            + candidate
                .reasons
                .iter()
                .map(|value| estimate_context_tokens(value))
                .sum::<usize>();
        if estimated_tokens + tokens > budget {
            truncated = true;
            false
        } else {
            estimated_tokens += tokens;
            true
        }
    });

    Ok(TaskContextPack {
        related_files: task.related_files.clone(),
        task,
        query,
        facts,
        documents,
        pending_candidates: candidates,
        estimated_tokens,
        budget_tokens: budget,
        truncated,
    })
}

pub fn record_task_decision(
    project_root: &Path,
    task_id: Option<&str>,
    statement: String,
    rationale: String,
    evidence: Vec<String>,
    confidence: u8,
) -> Result<(TaskRecord, ProjectFact)> {
    memory_record_decision(
        project_root,
        task_id,
        statement,
        rationale,
        evidence,
        confidence,
    )
}

pub fn record_task_failed_approach(
    project_root: &Path,
    task_id: Option<&str>,
    approach: String,
    reason: String,
    evidence: Vec<String>,
) -> Result<(TaskRecord, ProjectFact)> {
    memory_record_failed_approach(project_root, task_id, approach, reason, evidence)
}

pub fn checkpoint_task(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
    related_files: Vec<String>,
) -> Result<TaskRecord> {
    memory_checkpoint_task(project_root, task_id, summary, related_files)
}

pub fn reconcile_project_knowledge(
    project_root: &Path,
    task_id: Option<String>,
) -> Result<ReconciliationReport> {
    memory_reconcile_knowledge(project_root, task_id)
}

pub fn close_task(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
    reconcile: bool,
) -> Result<CloseTaskResult> {
    let task = memory_close_task(project_root, task_id, summary)?;
    let reconciliation = if reconcile {
        Some(memory_reconcile_knowledge(
            project_root,
            Some(task.id.clone()),
        )?)
    } else {
        None
    };
    Ok(CloseTaskResult {
        task,
        reconciliation,
    })
}

pub fn current_change_fingerprint(project_root: &Path) -> Result<String> {
    git_change_fingerprint(project_root)
}

pub fn current_change_snapshot(project_root: &Path) -> Result<GitChangeSnapshot> {
    git_change_snapshot(project_root)
}

pub fn watch_project_once(
    project_root: &Path,
    last_snapshot: Option<&GitChangeSnapshot>,
) -> Result<WatchTick> {
    ensure_directory(project_root)?;

    let snapshot = current_change_snapshot(project_root)?;
    if last_snapshot.map(|value| value.fingerprint.as_str()) == Some(snapshot.fingerprint.as_str())
    {
        return Ok(WatchTick {
            changed: false,
            diff_result: None,
            inbox_result: None,
            auto_applied_patches: 0,
        });
    }

    let diff_options = match last_snapshot {
        Some(previous) => DiffOptions::new(project_root.to_path_buf())
            .incremental(changed_paths_since(previous, &snapshot)),
        None => DiffOptions::new(project_root.to_path_buf()),
    };
    let diff_result = analyze_project_diff(diff_options)?;
    let inbox_result = generate_inbox(InboxGenerateOptions::new(
        project_root.to_path_buf(),
        Some(diff_result.analysis_path.clone()),
    ))?;
    let policy = load_or_default(project_root)?;
    let mut auto_applied_patches = 0;
    let has_non_documentation_changes = diff_result.analysis.changed_files.iter().any(|file| {
        let normalized = file.path.replace('\\', "/").to_lowercase();
        !normalized.starts_with("docs/")
            && normalized != "readme.md"
            && !normalized.starts_with(".cyclaw/")
    });
    if has_non_documentation_changes
        && policy.permissions.allow_docs_apply
        && policy.permissions.allow_auto_apply_docs
    {
        let draft_result = generate_document_drafts(
            DraftOptions::new(project_root.to_path_buf(), None, true).with_confidence(
                policy.automation.auto_apply_min_confidence,
                policy.permissions.allow_model_call && policy.permissions.allow_network,
            ),
        );
        if let Ok(draft_result) = draft_result {
            for patch in draft_result.patches {
                if apply_document_patch(project_root.to_path_buf(), &patch.id).is_ok() {
                    auto_applied_patches += 1;
                }
            }
        }
    }
    Ok(WatchTick {
        changed: true,
        diff_result: Some(diff_result),
        inbox_result: Some(inbox_result),
        auto_applied_patches,
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
        steps.push("运行 `cyclaw watch --once` 自动分析当前变更".to_string());
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

fn append_new_candidates(
    existing: &mut Vec<KnowledgeCandidate>,
    generated: &[KnowledgeCandidate],
) -> Vec<KnowledgeCandidate> {
    let existing_ids = existing
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    let mut added = Vec::new();
    for candidate in generated {
        if !existing_ids.contains(&candidate.id) {
            existing.push(candidate.clone());
            added.push(candidate.clone());
        }
    }
    added
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

fn record_event(
    project_root: &Path,
    event_type: AgentEventType,
    source: &str,
    summary: &str,
    data: serde_json::Value,
) -> Result<()> {
    append_event(project_root, &new_event(event_type, source, summary, data)).map(|_| ())
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

fn current_git_head(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn estimate_context_tokens(value: &str) -> usize {
    value.chars().count().div_ceil(3).max(1)
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

        cyclaw_policy::set_permission(temp.path(), "allow_docs_apply", true).unwrap();

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

        cyclaw_policy::set_permission(temp.path(), "allow_docs_apply", true).unwrap();

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
    fn applies_and_reverts_update_operation() {
        let (temp, candidate_id) = dependency_candidate_project();
        let target = temp.path().join("docs/dependencies.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let original = "# Dependencies\n\n## 旧依赖\n\n使用旧版本。\n";
        fs::write(&target, original).unwrap();

        let patch = generate_document_drafts(
            DraftOptions::new(temp.path().to_path_buf(), Some(candidate_id), false).with_operation(
                KnowledgeOperation::Update,
                Some("旧依赖".to_string()),
                Vec::new(),
                Some("## 新依赖\n\n使用新版本。".to_string()),
                false,
            ),
        )
        .unwrap()
        .patches
        .remove(0);
        apply_document_patch(temp.path().to_path_buf(), &patch.id).unwrap();
        assert!(fs::read_to_string(&target).unwrap().contains("使用新版本"));

        revert_document_patch(temp.path().to_path_buf(), &patch.id).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[test]
    fn deletes_and_restores_target_document() {
        let (temp, candidate_id) = dependency_candidate_project();
        let target = temp.path().join("docs/dependencies.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let original = "# Dependencies\n\n即将废弃。\n";
        fs::write(&target, original).unwrap();

        let patch =
            generate_document_drafts(
                DraftOptions::new(temp.path().to_path_buf(), Some(candidate_id), false)
                    .with_operation(KnowledgeOperation::Delete, None, Vec::new(), None, true),
            )
            .unwrap()
            .patches
            .remove(0);
        apply_document_patch(temp.path().to_path_buf(), &patch.id).unwrap();
        assert!(!target.exists());

        revert_document_patch(temp.path().to_path_buf(), &patch.id).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }

    #[test]
    fn rejects_stale_document_patch() {
        let (temp, candidate_id) = dependency_candidate_project();
        let target = temp.path().join("docs/dependencies.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "# Dependencies\n\n## 旧依赖\n\n旧内容。\n").unwrap();

        let patch = generate_document_drafts(
            DraftOptions::new(temp.path().to_path_buf(), Some(candidate_id), false).with_operation(
                KnowledgeOperation::Update,
                Some("旧依赖".to_string()),
                Vec::new(),
                Some("## 新依赖\n\n新内容。".to_string()),
                false,
            ),
        )
        .unwrap()
        .patches
        .remove(0);
        fs::write(&target, "# Dependencies\n\n开发者刚刚修改。\n").unwrap();

        let error = apply_document_patch(temp.path().to_path_buf(), &patch.id).unwrap_err();
        assert!(error.to_string().contains("草稿生成后已变化"));
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

    #[test]
    fn task_context_recalls_previous_session_decision() {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.email", "test@example.com"]);
        run_git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(temp.path().join("README.md"), "# Project\n").unwrap();
        run_git(temp.path(), &["add", "."]);
        run_git(temp.path(), &["commit", "-m", "initial"]);

        let first = begin_task(BeginTaskOptions::new(
            temp.path().to_path_buf(),
            "退款约束".to_string(),
            "确认退款状态规则".to_string(),
        ))
        .unwrap();
        record_task_decision(
            temp.path(),
            Some(&first.id),
            "退款完成后不得重新进入处理中".to_string(),
            "避免重复退款".to_string(),
            vec!["README.md".to_string()],
            95,
        )
        .unwrap();
        close_task(
            temp.path(),
            Some(&first.id),
            "已确认退款约束".to_string(),
            true,
        )
        .unwrap();

        let second = begin_task(BeginTaskOptions::new(
            temp.path().to_path_buf(),
            "修改退款状态机".to_string(),
            "调整处理中状态".to_string(),
        ))
        .unwrap();
        let context = get_task_context(
            temp.path(),
            Some(&second.id),
            Some("退款处理中"),
            Some(1_000),
            10,
        )
        .unwrap();

        assert!(
            context
                .facts
                .facts
                .iter()
                .any(|item| item.fact.statement.contains("不得重新进入"))
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

    fn dependency_candidate_project() -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.email", "test@example.com"]);
        run_git(temp.path(), &["config", "user.name", "Test User"]);
        cyclaw_policy::set_permission(temp.path(), "allow_docs_apply", true).unwrap();
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
        let diff = analyze_project_diff(DiffOptions::new(temp.path().to_path_buf())).unwrap();
        let inbox = generate_inbox(InboxGenerateOptions::new(
            temp.path().to_path_buf(),
            Some(diff.analysis_path),
        ))
        .unwrap();
        let candidate_id = inbox.generated[0].id.clone();
        update_inbox_status(
            temp.path().to_path_buf(),
            &candidate_id,
            KnowledgeStatus::Accepted,
        )
        .unwrap();
        (temp, candidate_id)
    }
}
