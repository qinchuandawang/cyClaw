use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cyclaw_events::{AgentEventType, append_event, new_event, new_id};
use cyclaw_policy::acquire_lock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

const CYCLE_DIR: &str = ".cyclaw";
const MAX_EVIDENCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const EVIDENCE_VERIFICATION_ROLL_BYTES: u64 = 4 * 1024 * 1024;
const EVIDENCE_VERIFICATION_ARCHIVE_LIMIT: usize = 12;
const MAX_IDENTIFIER_BYTES: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Active,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskDecision {
    pub id: String,
    pub statement: String,
    pub rationale: String,
    pub evidence: Vec<String>,
    pub confidence: u8,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedApproach {
    pub id: String,
    pub approach: String,
    pub reason: String,
    pub evidence: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskCheckpoint {
    pub id: String,
    pub summary: String,
    pub related_files: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    #[default]
    Investigate,
    Design,
    Implement,
    Verify,
    Handoff,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskActivity {
    pub kind: String,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub title: String,
    pub objective: String,
    pub status: TaskStatus,
    pub related_files: Vec<String>,
    pub context_budget_tokens: usize,
    #[serde(default)]
    pub phase: TaskPhase,
    #[serde(default)]
    pub phase_summary: String,
    #[serde(default)]
    pub recent_activity: Vec<TaskActivity>,
    pub decisions: Vec<TaskDecision>,
    pub failed_approaches: Vec<FailedApproach>,
    pub checkpoints: Vec<TaskCheckpoint>,
    pub summary: Option<String>,
    pub git_head: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BeginTaskOptions {
    pub project_root: PathBuf,
    pub title: String,
    pub objective: String,
    pub related_files: Vec<String>,
    pub context_budget_tokens: usize,
    pub phase: TaskPhase,
    pub git_head: Option<String>,
    pub session_id: Option<String>,
}

impl BeginTaskOptions {
    pub fn new(project_root: PathBuf, title: String, objective: String) -> Self {
        Self {
            project_root,
            title,
            objective,
            related_files: Vec::new(),
            context_budget_tokens: 2_000,
            phase: TaskPhase::default(),
            git_head: None,
            session_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    Decision,
    FailedApproach,
    Constraint,
    ApiContract,
    SchemaRule,
    Dependency,
    Environment,
    Architecture,
    Operational,
    Unknown,
}

impl std::str::FromStr for FactType {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "decision" => Ok(Self::Decision),
            "failed_approach" => Ok(Self::FailedApproach),
            "constraint" => Ok(Self::Constraint),
            "api_contract" => Ok(Self::ApiContract),
            "schema_rule" => Ok(Self::SchemaRule),
            "dependency" => Ok(Self::Dependency),
            "environment" => Ok(Self::Environment),
            "architecture" => Ok(Self::Architecture),
            "operational" => Ok(Self::Operational),
            "unknown" => Ok(Self::Unknown),
            _ => anyhow::bail!("未知事实类型: {}", value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Active,
    Superseded,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFact {
    pub id: String,
    pub statement: String,
    pub fact_type: FactType,
    pub status: FactStatus,
    pub evidence: Vec<String>,
    /// 保留旧版路径数组，并为新写入同步结构化证据。
    #[serde(default)]
    pub evidence_details: Vec<FactEvidence>,
    pub source_task_id: Option<String>,
    pub confidence: u8,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub supersedes: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_verified_at: String,
}

/// 可定位且可验证的事实证据。旧版 `evidence: Vec<String>` 仍可被读取。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactEvidence {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_scope: Option<EvidenceHashScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_head: Option<String>,
    #[serde(default)]
    pub captured_at: String,
    #[serde(default)]
    pub verified_at: String,
    #[serde(default = "default_evidence_type")]
    pub evidence_type: String,
}

fn default_evidence_type() -> String {
    "file".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceHashScope {
    File,
    LineRange,
    Symbol,
}

impl std::str::FromStr for EvidenceHashScope {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "file" => Ok(Self::File),
            "line_range" => Ok(Self::LineRange),
            "symbol" => Ok(Self::Symbol),
            _ => anyhow::bail!("未知证据哈希范围: {}", value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceVerificationStatus {
    Verified,
    Missing,
    HashMismatch,
    InvalidLocation,
    OutsideProject,
    Unsupported,
    TooLarge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactEvidenceVerification {
    pub path: String,
    pub evidence_type: String,
    pub status: EvidenceVerificationStatus,
    pub expected_hash: Option<String>,
    pub actual_hash: Option<String>,
    pub checked_at: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactVerificationReport {
    pub fact_id: String,
    pub results: Vec<FactEvidenceVerification>,
    pub verified_count: usize,
    pub issue_count: usize,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceVerificationRecord {
    pub id: String,
    pub fact_id: String,
    pub git_head: Option<String>,
    pub results: Vec<FactEvidenceVerification>,
    pub verified_count: usize,
    pub issue_count: usize,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceVerificationIssue {
    pub path: String,
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceVerificationPage {
    pub records: Vec<EvidenceVerificationRecord>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub issues: Vec<EvidenceVerificationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EvidenceVerificationArchiveIndex {
    schema_version: u32,
    source_file: String,
    source_size: u64,
    source_modified_ns: Option<u128>,
    entries: Vec<EvidenceVerificationArchiveEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EvidenceVerificationArchiveEntry {
    fact_id: String,
    checked_at: String,
    offset: u64,
    length: u64,
}

#[derive(Debug, Clone)]
enum EvidenceVerificationCandidate {
    Loaded(EvidenceVerificationRecord),
    Indexed {
        archive_path: PathBuf,
        entry: EvidenceVerificationArchiveEntry,
    },
}

impl EvidenceVerificationCandidate {
    fn fact_id(&self) -> &str {
        match self {
            Self::Loaded(record) => &record.fact_id,
            Self::Indexed { entry, .. } => &entry.fact_id,
        }
    }

    fn checked_at(&self) -> &str {
        match self {
            Self::Loaded(record) => &record.checked_at,
            Self::Indexed { entry, .. } => &entry.checked_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactInput {
    pub statement: String,
    pub fact_type: FactType,
    pub evidence: Vec<String>,
    pub evidence_details: Vec<FactEvidence>,
    pub source_task_id: Option<String>,
    pub confidence: u8,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactOperation {
    Create,
    Update,
    Merge,
    Supersede,
    Delete,
}

impl std::str::FromStr for FactOperation {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "merge" => Ok(Self::Merge),
            "supersede" => Ok(Self::Supersede),
            "delete" => Ok(Self::Delete),
            _ => anyhow::bail!("未知事实操作: {}", value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactPatchStatus {
    Pending,
    Applying,
    Applied,
    Reverting,
    Reverted,
}

impl std::str::FromStr for FactPatchStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "reverting" => Ok(Self::Reverting),
            "reverted" => Ok(Self::Reverted),
            _ => anyhow::bail!("未知事实草稿状态: {}", value),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FactTransactionOperation {
    Apply,
    Revert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FactPatchTransaction {
    patch_id: String,
    operation: FactTransactionOperation,
    started_at: String,
    before_fingerprint: String,
    after_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactTransactionDiagnosticStatus {
    RecoverableBefore,
    RecoverableAfter,
    Invalid,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactTransactionDiagnostic {
    pub patch_id: Option<String>,
    pub path: String,
    pub operation: Option<String>,
    pub status: FactTransactionDiagnosticStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FactTransactionDiagnostics {
    pub pending_count: usize,
    pub recoverable_count: usize,
    pub blocked_count: usize,
    pub transactions: Vec<FactTransactionDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactRecoveryFailure {
    pub path: String,
    pub patch_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FactRecoveryReport {
    #[serde(default)]
    pub checked_at: String,
    pub recovered: Vec<String>,
    pub failures: Vec<FactRecoveryFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactPatchAuditEvent {
    pub action: String,
    pub created_at: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactPatch {
    pub id: String,
    pub operation: FactOperation,
    pub target_fact_id: Option<String>,
    #[serde(default)]
    pub source_fact_ids: Vec<String>,
    pub before: Vec<ProjectFact>,
    pub after: Vec<ProjectFact>,
    pub preview_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fingerprint: Option<String>,
    pub status: FactPatchStatus,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at: Option<String>,
    #[serde(default)]
    pub audit_events: Vec<FactPatchAuditEvent>,
}

#[derive(Debug, Clone)]
pub struct FactPatchRequest {
    pub operation: FactOperation,
    pub target_fact_id: Option<String>,
    pub source_fact_ids: Vec<String>,
    pub fact: Option<ProjectFact>,
}

#[derive(Debug, Clone, Default)]
pub struct FactPatchQuery {
    pub status: Option<FactPatchStatus>,
    pub operation: Option<FactOperation>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactPatchPage {
    pub patches: Vec<FactPatch>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactContextItem {
    pub fact: ProjectFact,
    pub relevance_score: u32,
    pub relevance_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactContext {
    pub query: String,
    pub facts: Vec<FactContextItem>,
    pub estimated_tokens: usize,
    pub budget_tokens: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationKind {
    Duplicate,
    Conflict,
    Stale,
    EvidenceDrift,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciliationFinding {
    pub id: String,
    pub kind: ReconciliationKind,
    pub fact_ids: Vec<String>,
    pub reason: String,
    pub recommended_operation: String,
    pub confidence: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub id: String,
    pub task_id: Option<String>,
    pub findings: Vec<ReconciliationFinding>,
    pub duplicate_count: usize,
    pub conflict_count: usize,
    pub stale_count: usize,
    #[serde(default)]
    pub drift_count: usize,
    pub created_at: String,
}

pub fn begin_task(options: BeginTaskOptions) -> Result<TaskRecord> {
    ensure_memory_dirs(&options.project_root)?;
    let _lock = acquire_lock(&options.project_root, "memory", Duration::from_secs(5))?;
    if let Some(active) =
        read_active_task_id_for_session(&options.project_root, options.session_id.as_deref())?
    {
        anyhow::bail!("已有活动任务，请先关闭: {}", active);
    }
    let now = Utc::now().to_rfc3339();
    let task = TaskRecord {
        id: new_id("task"),
        session_id: options.session_id.clone(),
        title: options.title,
        objective: options.objective,
        status: TaskStatus::Active,
        related_files: dedupe_strings(options.related_files),
        context_budget_tokens: options.context_budget_tokens.clamp(256, 16_000),
        phase: options.phase,
        phase_summary: String::new(),
        recent_activity: Vec::new(),
        decisions: Vec::new(),
        failed_approaches: Vec::new(),
        checkpoints: Vec::new(),
        summary: None,
        git_head: options.git_head,
        created_at: now.clone(),
        updated_at: now,
        closed_at: None,
    };
    write_task(&options.project_root, &task)?;
    write_active_task_id(
        &options.project_root,
        options.session_id.as_deref(),
        &task.id,
    )?;
    record_event(
        &options.project_root,
        AgentEventType::TaskStarted,
        "开始项目任务",
        serde_json::json!({"task_id":task.id,"title":task.title}),
    )?;
    Ok(task)
}

pub fn get_active_task(project_root: &Path) -> Result<Option<TaskRecord>> {
    get_active_task_for_session(project_root, None)
}

pub fn get_active_task_for_session(
    project_root: &Path,
    session_id: Option<&str>,
) -> Result<Option<TaskRecord>> {
    let Some(id) = read_active_task_id_for_session(project_root, session_id)? else {
        return Ok(None);
    };
    Ok(Some(read_task(project_root, &id)?))
}

pub fn get_task(project_root: &Path, task_id: &str) -> Result<TaskRecord> {
    read_task(project_root, task_id)
}

pub fn set_task_phase(
    project_root: &Path,
    task_id: Option<&str>,
    phase: TaskPhase,
) -> Result<TaskRecord> {
    set_task_phase_for_session(project_root, task_id, phase, None)
}

pub fn set_task_phase_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    phase: TaskPhase,
    session_id: Option<&str>,
) -> Result<TaskRecord> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id_for_session(project_root, task_id, session_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_task_session(&task, session_id)?;
    ensure_active(&task)?;
    task.phase = phase;
    let now = Utc::now().to_rfc3339();
    let activity_summary = format!("进入 {:?} 阶段", task.phase);
    push_task_activity(&mut task, "phase", activity_summary, now.clone());
    task.updated_at = now;
    write_task(project_root, &task)?;
    Ok(task)
}

pub fn list_tasks(project_root: &Path, limit: usize) -> Result<Vec<TaskRecord>> {
    let dir = tasks_dir(project_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut tasks = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|content| serde_json::from_str::<TaskRecord>(&content).ok())
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    tasks.truncate(limit.clamp(1, 100));
    Ok(tasks)
}

pub fn record_decision(
    project_root: &Path,
    task_id: Option<&str>,
    statement: String,
    rationale: String,
    evidence: Vec<String>,
    confidence: u8,
) -> Result<(TaskRecord, ProjectFact)> {
    record_decision_for_session(
        project_root,
        task_id,
        statement,
        rationale,
        evidence,
        confidence,
        None,
    )
}

pub fn record_decision_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    statement: String,
    rationale: String,
    evidence: Vec<String>,
    confidence: u8,
    session_id: Option<&str>,
) -> Result<(TaskRecord, ProjectFact)> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id_for_session(project_root, task_id, session_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_task_session(&task, session_id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    let decision = TaskDecision {
        id: item_id("decision", &statement, &now),
        statement: statement.clone(),
        rationale,
        evidence: dedupe_strings(evidence.clone()),
        confidence: confidence.min(100),
        created_at: now.clone(),
    };
    task.decisions.push(decision);
    push_task_activity(
        &mut task,
        "decision",
        "记录关键决策".to_string(),
        now.clone(),
    );
    task.updated_at = now.clone();
    write_task(project_root, &task)?;
    let fact = upsert_fact(
        project_root,
        ProjectFact {
            id: item_id("fact", &statement, &id),
            statement,
            fact_type: FactType::Decision,
            status: FactStatus::Active,
            evidence: dedupe_strings(evidence),
            evidence_details: Vec::new(),
            source_task_id: Some(id.clone()),
            confidence: confidence.min(100),
            valid_from: task.git_head.clone(),
            valid_until: None,
            supersedes: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_verified_at: now,
        },
    )?;
    record_event(
        project_root,
        AgentEventType::FactRecorded,
        "记录任务决策",
        serde_json::json!({"task_id":id,"fact_id":fact.id}),
    )?;
    Ok((task, fact))
}

pub fn record_failed_approach(
    project_root: &Path,
    task_id: Option<&str>,
    approach: String,
    reason: String,
    evidence: Vec<String>,
) -> Result<(TaskRecord, ProjectFact)> {
    record_failed_approach_for_session(project_root, task_id, approach, reason, evidence, None)
}

pub fn record_failed_approach_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    approach: String,
    reason: String,
    evidence: Vec<String>,
    session_id: Option<&str>,
) -> Result<(TaskRecord, ProjectFact)> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id_for_session(project_root, task_id, session_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_task_session(&task, session_id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    let statement = format!("失败方案：{}；原因：{}", approach, reason);
    task.failed_approaches.push(FailedApproach {
        id: item_id("failure", &approach, &now),
        approach,
        reason,
        evidence: dedupe_strings(evidence.clone()),
        created_at: now.clone(),
    });
    push_task_activity(
        &mut task,
        "failure",
        "记录失败方案".to_string(),
        now.clone(),
    );
    task.updated_at = now.clone();
    write_task(project_root, &task)?;
    let fact = upsert_fact(
        project_root,
        ProjectFact {
            id: item_id("fact", &statement, &id),
            statement,
            fact_type: FactType::FailedApproach,
            status: FactStatus::Active,
            evidence: dedupe_strings(evidence),
            evidence_details: Vec::new(),
            source_task_id: Some(id.clone()),
            confidence: 95,
            valid_from: task.git_head.clone(),
            valid_until: None,
            supersedes: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_verified_at: now,
        },
    )?;
    record_event(
        project_root,
        AgentEventType::FactRecorded,
        "记录失败方案",
        serde_json::json!({"task_id":id,"fact_id":fact.id}),
    )?;
    Ok((task, fact))
}

pub fn checkpoint_task(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
    related_files: Vec<String>,
) -> Result<TaskRecord> {
    checkpoint_task_for_session(project_root, task_id, summary, related_files, None)
}

pub fn checkpoint_task_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
    related_files: Vec<String>,
    session_id: Option<&str>,
) -> Result<TaskRecord> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id_for_session(project_root, task_id, session_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_task_session(&task, session_id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    let files = dedupe_strings(related_files);
    task.related_files.extend(files.clone());
    task.related_files = dedupe_strings(task.related_files);
    task.checkpoints.push(TaskCheckpoint {
        id: item_id("checkpoint", &summary, &now),
        summary: summary.clone(),
        related_files: files,
        created_at: now.clone(),
    });
    push_task_activity(&mut task, "checkpoint", summary, now.clone());
    task.updated_at = now;
    write_task(project_root, &task)?;
    record_event(
        project_root,
        AgentEventType::TaskCheckpointed,
        "记录任务检查点",
        serde_json::json!({"task_id":id,"checkpoint_count":task.checkpoints.len()}),
    )?;
    Ok(task)
}

pub fn close_task(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
) -> Result<TaskRecord> {
    close_task_for_session(project_root, task_id, summary, None)
}

pub fn close_task_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    summary: String,
    session_id: Option<&str>,
) -> Result<TaskRecord> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id_for_session(project_root, task_id, session_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_task_session(&task, session_id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    task.status = TaskStatus::Closed;
    task.summary = Some(summary);
    task.updated_at = now.clone();
    task.closed_at = Some(now);
    write_task(project_root, &task)?;
    if read_active_task_id_for_session(project_root, task.session_id.as_deref())?.as_deref()
        == Some(id.as_str())
    {
        let path = active_task_path_for_session(project_root, task.session_id.as_deref());
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    record_event(
        project_root,
        AgentEventType::TaskClosed,
        "关闭项目任务",
        serde_json::json!({"task_id":id,"decisions":task.decisions.len(),"failures":task.failed_approaches.len()}),
    )?;
    Ok(task)
}

pub fn list_facts(project_root: &Path) -> Result<Vec<ProjectFact>> {
    let path = facts_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(serde_json::from_str::<ProjectFact>(line)?))
        .collect()
}

pub fn project_fact_from_input(input: FactInput) -> ProjectFact {
    let now = Utc::now().to_rfc3339();
    let mut fact = ProjectFact {
        id: String::new(),
        statement: input.statement,
        fact_type: input.fact_type,
        status: FactStatus::Active,
        evidence: input.evidence,
        evidence_details: input.evidence_details,
        source_task_id: input.source_task_id,
        confidence: input.confidence.min(100),
        valid_from: input.valid_from,
        valid_until: input.valid_until,
        supersedes: Vec::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
        last_verified_at: now.clone(),
    };
    normalize_fact(&mut fact, &now);
    fact
}

pub fn verify_fact_evidence(project_root: &Path, fact_id: &str) -> Result<FactVerificationReport> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    validate_fact_identifier(fact_id)?;
    let fact = find_fact(&list_facts(project_root)?, fact_id)?;
    let report = verify_fact(project_root, &fact);
    append_evidence_verification(project_root, &report)?;
    Ok(report)
}

pub fn list_evidence_verifications(
    project_root: &Path,
    fact_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<Vec<EvidenceVerificationRecord>> {
    Ok(query_evidence_verifications(project_root, fact_id, offset, limit)?.records)
}

pub fn query_evidence_verifications(
    project_root: &Path,
    fact_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<EvidenceVerificationPage> {
    if let Some(fact_id) = fact_id {
        validate_fact_identifier(fact_id)?;
    }
    let paths = evidence_verification_paths(project_root)?;
    let active_path = evidence_verifications_path(project_root);
    let mut candidates = Vec::new();
    let mut issues = Vec::new();
    for path in paths {
        if let Some(index) = read_evidence_verification_archive_index(&path)? {
            candidates.extend(index.entries.into_iter().map(|entry| {
                EvidenceVerificationCandidate::Indexed {
                    archive_path: path.clone(),
                    entry,
                }
            }));
        } else {
            let (records, path_issues) = read_evidence_verification_file(&path, false)?;
            candidates.extend(
                records
                    .into_iter()
                    .map(EvidenceVerificationCandidate::Loaded),
            );
            issues.extend(path_issues);
        }
    }
    if active_path.exists() {
        let (records, path_issues) = read_evidence_verification_file(&active_path, true)?;
        candidates.extend(
            records
                .into_iter()
                .map(EvidenceVerificationCandidate::Loaded),
        );
        issues.extend(path_issues);
    }
    candidates.retain(|candidate| fact_id.is_none_or(|fact_id| candidate.fact_id() == fact_id));
    candidates.sort_by(|left, right| right.checked_at().cmp(left.checked_at()));
    let total = candidates.len();
    let limit = limit.clamp(1, 200);
    let records = candidates
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(materialize_evidence_verification_candidate)
        .collect::<Result<Vec<_>>>()?;
    Ok(EvidenceVerificationPage {
        records,
        total,
        offset,
        limit,
        issues,
    })
}

fn materialize_evidence_verification_candidate(
    candidate: EvidenceVerificationCandidate,
) -> Result<EvidenceVerificationRecord> {
    match candidate {
        EvidenceVerificationCandidate::Loaded(record) => Ok(record),
        EvidenceVerificationCandidate::Indexed {
            archive_path,
            entry,
        } => read_indexed_evidence_verification(&archive_path, &entry),
    }
}

fn read_evidence_verification_file(
    path: &Path,
    tolerate_incomplete_tail: bool,
) -> Result<(
    Vec<EvidenceVerificationRecord>,
    Vec<EvidenceVerificationIssue>,
)> {
    let content = fs::read_to_string(path)?;
    let ends_with_newline = content.ends_with('\n');
    let lines = content.lines().collect::<Vec<_>>();
    let mut records = Vec::new();
    let mut issues = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<EvidenceVerificationRecord>(line) {
            Ok(record) => records.push(record),
            Err(error)
                if tolerate_incomplete_tail && index + 1 == lines.len() && !ends_with_newline =>
            {
                issues.push(EvidenceVerificationIssue {
                    path: path.display().to_string(),
                    line: index + 1,
                    reason: format!("忽略进程中断留下的不完整末行: {}", error),
                });
            }
            Err(error) => {
                anyhow::bail!(
                    "证据验证账本损坏: {}:{}: {}",
                    path.display(),
                    index + 1,
                    error
                );
            }
        }
    }
    Ok((records, issues))
}

fn verify_fact(project_root: &Path, fact: &ProjectFact) -> FactVerificationReport {
    let checked_at = Utc::now().to_rfc3339();
    let evidence = if fact.evidence_details.is_empty() {
        fact.evidence
            .iter()
            .map(|path| FactEvidence {
                path: path.clone(),
                symbol: None,
                line_start: None,
                line_end: None,
                content_hash: None,
                hash_scope: None,
                git_head: None,
                captured_at: fact.created_at.clone(),
                verified_at: fact.last_verified_at.clone(),
                evidence_type: "file".to_string(),
            })
            .collect::<Vec<_>>()
    } else {
        fact.evidence_details.clone()
    };
    let results = evidence
        .iter()
        .map(|item| verify_evidence(project_root, item, &checked_at))
        .collect::<Vec<_>>();
    let verified_count = results
        .iter()
        .filter(|result| result.status == EvidenceVerificationStatus::Verified)
        .count();
    FactVerificationReport {
        fact_id: fact.id.clone(),
        issue_count: results.len().saturating_sub(verified_count),
        results,
        verified_count,
        checked_at,
    }
}

fn verify_evidence(
    project_root: &Path,
    evidence: &FactEvidence,
    checked_at: &str,
) -> FactEvidenceVerification {
    let base = FactEvidenceVerification {
        path: evidence.path.clone(),
        evidence_type: evidence.evidence_type.clone(),
        status: EvidenceVerificationStatus::Unsupported,
        expected_hash: evidence.content_hash.clone(),
        actual_hash: None,
        checked_at: checked_at.to_string(),
        reason: String::new(),
    };
    if !matches!(
        evidence.evidence_type.as_str(),
        "file" | "source" | "config"
    ) {
        return FactEvidenceVerification {
            reason: format!("暂不支持验证证据类型: {}", evidence.evidence_type),
            ..base
        };
    }
    let path = resolve_evidence_path(project_root, &evidence.path);
    let Ok(path) = path else {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::OutsideProject,
            reason: "证据路径位于项目根目录之外".to_string(),
            ..base
        };
    };
    if !path.is_file() {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::Missing,
            reason: "证据文件不存在".to_string(),
            ..base
        };
    }
    let Ok(metadata) = fs::metadata(&path) else {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::Missing,
            reason: "证据文件无法读取".to_string(),
            ..base
        };
    };
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::TooLarge,
            reason: format!(
                "证据文件超过 {} MiB 验证上限",
                MAX_EVIDENCE_FILE_BYTES / 1024 / 1024
            ),
            ..base
        };
    }
    let needs_content = evidence.symbol.is_some()
        || evidence.line_start.is_some()
        || !matches!(evidence.hash_scope, None | Some(EvidenceHashScope::File));
    let content = if needs_content {
        match fs::read(&path) {
            Ok(content) => Some(content),
            Err(_) => {
                return FactEvidenceVerification {
                    status: EvidenceVerificationStatus::Missing,
                    reason: "证据文件无法读取".to_string(),
                    ..base
                };
            }
        }
    } else {
        None
    };
    if content.as_deref().is_some_and(|content| {
        !valid_evidence_location(&String::from_utf8_lossy(content), evidence)
    }) {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::InvalidLocation,
            reason: "证据符号或行范围已无法定位".to_string(),
            ..base
        };
    }
    let Ok(actual_hash) = hash_evidence_content(&path, evidence, content.as_deref()) else {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::InvalidLocation,
            reason: "无法按指定 hash_scope 提取证据内容".to_string(),
            ..base
        };
    };
    if evidence
        .content_hash
        .as_deref()
        .is_some_and(|expected| normalize_hash(expected) != normalize_hash(&actual_hash))
    {
        return FactEvidenceVerification {
            status: EvidenceVerificationStatus::HashMismatch,
            actual_hash: Some(actual_hash),
            reason: "证据内容哈希与采集时不一致".to_string(),
            ..base
        };
    }
    FactEvidenceVerification {
        status: EvidenceVerificationStatus::Verified,
        actual_hash: Some(actual_hash),
        reason: "证据仍可定位且内容校验通过".to_string(),
        ..base
    }
}

pub fn preview_fact_patch(project_root: &Path, request: FactPatchRequest) -> Result<FactPatch> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    ensure_recovery_clean(recover_fact_patch_transactions_locked(project_root)?)?;
    if let Some(id) = request.target_fact_id.as_deref() {
        validate_fact_identifier(id)?;
    }
    for id in &request.source_fact_ids {
        validate_fact_identifier(id)?;
    }
    let facts = list_facts(project_root)?;
    let now = Utc::now().to_rfc3339();
    let target = request
        .target_fact_id
        .as_deref()
        .map(|id| find_fact(&facts, id))
        .transpose()?;
    let mut before = target.clone().into_iter().collect::<Vec<_>>();
    let mut after = Vec::new();

    match request.operation {
        FactOperation::Create => {
            let mut fact = request.fact.context("create 必须提供事实内容")?;
            if fact.statement.trim().is_empty() {
                anyhow::bail!("create 的事实陈述不能为空");
            }
            if facts.iter().any(|existing| {
                existing.status == FactStatus::Active
                    && existing.statement.trim() == fact.statement.trim()
            }) {
                anyhow::bail!("已存在相同的 Active Fact，请使用 update 或 merge");
            }
            if fact.id.trim().is_empty() {
                fact.id = item_id("fact", &fact.statement, &now);
            } else if facts.iter().any(|existing| existing.id == fact.id) {
                anyhow::bail!("Fact ID 已存在: {}", fact.id);
            }
            validate_new_fact_identifier(&fact.id)?;
            fact.status = FactStatus::Active;
            prepare_fact(project_root, &mut fact, &now);
            after.push(fact);
        }
        FactOperation::Update => {
            let old = target.context("update 必须指定 target_fact_id")?;
            ensure_active_fact(&old, "update")?;
            let mut fact = request.fact.context("update 必须提供事实内容")?;
            fact.id = old.id.clone();
            fact.created_at = old.created_at.clone();
            fact.status = FactStatus::Active;
            prepare_fact(project_root, &mut fact, &now);
            after.push(fact);
        }
        FactOperation::Delete => {
            let mut fact = target.context("delete 必须指定 target_fact_id")?;
            ensure_active_fact(&fact, "delete")?;
            fact.status = FactStatus::Deleted;
            fact.updated_at = now.clone();
            after.push(fact);
        }
        FactOperation::Supersede => {
            let old = target.context("supersede 必须指定 target_fact_id")?;
            ensure_active_fact(&old, "supersede")?;
            let mut replacement = request.fact.context("supersede 必须提供替代事实")?;
            if replacement.id.trim().is_empty() {
                replacement.id = item_id("fact", &replacement.statement, &now);
            }
            validate_new_fact_identifier(&replacement.id)?;
            if facts.iter().any(|fact| fact.id == replacement.id) {
                anyhow::bail!("替代 Fact ID 已存在: {}", replacement.id);
            }
            replacement.status = FactStatus::Active;
            replacement.supersedes = dedupe_strings(
                replacement
                    .supersedes
                    .into_iter()
                    .chain([old.id.clone()])
                    .collect(),
            );
            prepare_fact(project_root, &mut replacement, &now);
            let mut old = old;
            old.status = FactStatus::Superseded;
            old.updated_at = now.clone();
            after.extend([old, replacement]);
        }
        FactOperation::Merge => {
            let primary = target.context("merge 必须指定保留的 target_fact_id")?;
            ensure_active_fact(&primary, "merge")?;
            let source_ids = dedupe_strings(request.source_fact_ids.clone());
            if source_ids.iter().any(|id| id == &primary.id) {
                anyhow::bail!("merge 的 source_fact_ids 不能包含主 Fact ID");
            }
            let sources = request
                .source_fact_ids
                .iter()
                .filter(|id| **id != primary.id)
                .map(|id| find_fact(&facts, id))
                .collect::<Result<Vec<_>>>()?;
            if sources.is_empty() {
                anyhow::bail!("merge 至少需要一个待合并 source_fact_id");
            }
            if source_ids.len() != request.source_fact_ids.len() {
                anyhow::bail!("merge 的 source_fact_ids 不能重复");
            }
            for source in &sources {
                ensure_active_fact(source, "merge")?;
            }
            before.extend(sources.clone());
            let mut retained = request.fact.unwrap_or(primary.clone());
            retained.id = primary.id.clone();
            retained.created_at = primary.created_at.clone();
            retained.status = FactStatus::Active;
            retained.supersedes = dedupe_strings(
                retained
                    .supersedes
                    .into_iter()
                    .chain(sources.iter().map(|fact| fact.id.clone()))
                    .collect(),
            );
            retained.evidence = dedupe_strings(
                retained
                    .evidence
                    .into_iter()
                    .chain(primary.evidence.iter().cloned())
                    .chain(
                        sources
                            .iter()
                            .flat_map(|fact| fact.evidence.iter().cloned()),
                    )
                    .collect(),
            );
            retained.evidence_details = dedupe_evidence(
                retained
                    .evidence_details
                    .into_iter()
                    .chain(primary.evidence_details.iter().cloned())
                    .chain(
                        sources
                            .iter()
                            .flat_map(|fact| fact.evidence_details.iter().cloned()),
                    )
                    .collect(),
            );
            retained.confidence = sources.iter().fold(
                retained.confidence.max(primary.confidence),
                |value, fact| value.max(fact.confidence),
            );
            prepare_fact(project_root, &mut retained, &now);
            after.push(retained);
            for mut source in sources {
                source.status = FactStatus::Superseded;
                source.updated_at = now.clone();
                after.push(source);
            }
        }
    }

    let preview_fingerprint = facts_fingerprint(&before);
    let id = item_id("fact_patch", &format!("{:?}", request.operation), &now);
    let patch = FactPatch {
        id,
        operation: request.operation,
        target_fact_id: request.target_fact_id,
        source_fact_ids: dedupe_strings(request.source_fact_ids),
        before,
        after,
        preview_fingerprint,
        applied_fingerprint: None,
        status: FactPatchStatus::Pending,
        created_at: now.clone(),
        applied_at: None,
        reverted_at: None,
        audit_events: vec![FactPatchAuditEvent {
            action: "previewed".to_string(),
            created_at: now,
            detail: "已生成事实治理预览".to_string(),
        }],
    };
    write_fact_patch(project_root, &patch)?;
    record_event(
        project_root,
        AgentEventType::FactPatchCreated,
        "生成事实治理草稿",
        serde_json::json!({"patch_id":patch.id,"operation":patch.operation}),
    )?;
    Ok(patch)
}

pub fn list_fact_patches(project_root: &Path) -> Result<Vec<FactPatch>> {
    let _ = recover_fact_patch_transactions(project_root)?;
    list_fact_patches_raw(project_root)
}

pub fn query_fact_patches(project_root: &Path, query: FactPatchQuery) -> Result<FactPatchPage> {
    let mut patches = list_fact_patches(project_root)?;
    patches.retain(|patch| {
        query
            .status
            .as_ref()
            .is_none_or(|status| &patch.status == status)
            && query
                .operation
                .as_ref()
                .is_none_or(|operation| &patch.operation == operation)
    });
    patches.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    let total = patches.len();
    let limit = if query.limit == 0 {
        50
    } else {
        query.limit.clamp(1, 200)
    };
    Ok(FactPatchPage {
        patches: patches.into_iter().skip(query.offset).take(limit).collect(),
        total,
        offset: query.offset,
        limit,
    })
}

fn list_fact_patches_raw(project_root: &Path) -> Result<Vec<FactPatch>> {
    let dir = fact_patches_dir(project_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut patches = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .map(|path| {
            let patch_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .context("事实草稿文件名不是有效 UTF-8")?;
            read_fact_patch(project_root, patch_id)
        })
        .collect::<Result<Vec<_>>>()?;
    patches.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(patches)
}

pub fn apply_fact_patch(project_root: &Path, patch_id: &str) -> Result<FactPatch> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    validate_patch_identifier(patch_id)?;
    ensure_recovery_clean(recover_fact_patch_transactions_locked(project_root)?)?;
    let mut patch = read_fact_patch(project_root, patch_id)?;
    if patch.status != FactPatchStatus::Pending {
        anyhow::bail!("事实草稿不是待应用状态: {}", patch_id);
    }
    let mut facts = list_facts(project_root)?;
    let current = facts_for_ids(&facts, patch.before.iter().map(|fact| fact.id.as_str()))?;
    if facts_fingerprint(&current) != patch.preview_fingerprint {
        anyhow::bail!("事实在草稿预览后已变化，请重新生成草稿后再应用");
    }
    let now = Utc::now().to_rfc3339();
    let transaction = FactPatchTransaction {
        patch_id: patch.id.clone(),
        operation: FactTransactionOperation::Apply,
        started_at: now.clone(),
        before_fingerprint: facts_fingerprint(&patch.before),
        after_fingerprint: facts_fingerprint(&patch.after),
    };
    write_fact_transaction(project_root, &transaction)?;
    patch.status = FactPatchStatus::Applying;
    patch.audit_events.push(FactPatchAuditEvent {
        action: "apply_started".to_string(),
        created_at: now.clone(),
        detail: "已创建事务日志并开始应用事实治理草稿".to_string(),
    });
    write_fact_patch(project_root, &patch)?;
    replace_facts(&mut facts, &patch.after);
    write_facts(project_root, &facts)?;
    finalize_applied_patch(project_root, &mut patch, false)?;
    remove_fact_transaction(project_root, patch_id)?;
    record_event(
        project_root,
        AgentEventType::FactPatchApplied,
        "应用事实治理草稿",
        serde_json::json!({"patch_id":patch.id,"operation":patch.operation}),
    )?;
    Ok(patch)
}

pub fn revert_fact_patch(project_root: &Path, patch_id: &str) -> Result<FactPatch> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    validate_patch_identifier(patch_id)?;
    ensure_recovery_clean(recover_fact_patch_transactions_locked(project_root)?)?;
    let mut patch = read_fact_patch(project_root, patch_id)?;
    if patch.status != FactPatchStatus::Applied {
        anyhow::bail!("只有已应用的事实草稿可以撤销: {}", patch_id);
    }
    let mut facts = list_facts(project_root)?;
    let current = facts_for_ids(&facts, patch.after.iter().map(|fact| fact.id.as_str()))?;
    if patch.applied_fingerprint.as_deref() != Some(facts_fingerprint(&current).as_str()) {
        anyhow::bail!("事实在草稿应用后已变化，拒绝撤销以避免覆盖新内容");
    }
    let now = Utc::now().to_rfc3339();
    let transaction = FactPatchTransaction {
        patch_id: patch.id.clone(),
        operation: FactTransactionOperation::Revert,
        started_at: now.clone(),
        before_fingerprint: facts_fingerprint(&patch.after),
        after_fingerprint: facts_fingerprint(&patch.before),
    };
    write_fact_transaction(project_root, &transaction)?;
    patch.status = FactPatchStatus::Reverting;
    patch.audit_events.push(FactPatchAuditEvent {
        action: "revert_started".to_string(),
        created_at: now,
        detail: "已创建事务日志并开始撤销事实治理草稿".to_string(),
    });
    write_fact_patch(project_root, &patch)?;
    replace_facts(&mut facts, &patch.before);
    let before_ids = patch
        .before
        .iter()
        .map(|fact| fact.id.as_str())
        .collect::<HashSet<_>>();
    let after_only = patch
        .after
        .iter()
        .filter(|fact| !before_ids.contains(fact.id.as_str()))
        .map(|fact| fact.id.as_str())
        .collect::<Vec<_>>();
    facts.retain(|fact| !after_only.contains(&fact.id.as_str()));
    write_facts(project_root, &facts)?;
    finalize_reverted_patch(project_root, &mut patch, false)?;
    remove_fact_transaction(project_root, patch_id)?;
    record_event(
        project_root,
        AgentEventType::FactPatchReverted,
        "撤销事实治理草稿",
        serde_json::json!({"patch_id":patch.id,"operation":patch.operation}),
    )?;
    Ok(patch)
}

/// 恢复因进程中断而停留在跨文件事务中的 Fact Patch。
pub fn recover_fact_patch_transactions(project_root: &Path) -> Result<FactRecoveryReport> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    recover_fact_patch_transactions_locked(project_root)
}

fn recover_fact_patch_transactions_locked(project_root: &Path) -> Result<FactRecoveryReport> {
    let dir = fact_transactions_dir(project_root);
    if !dir.exists() {
        return Ok(FactRecoveryReport::default());
    }
    let mut paths = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut report = FactRecoveryReport {
        checked_at: Utc::now().to_rfc3339(),
        ..FactRecoveryReport::default()
    };
    for path in paths {
        let result = (|| -> Result<String> {
            let transaction =
                serde_json::from_str::<FactPatchTransaction>(&fs::read_to_string(&path)?)?;
            validate_transaction_path(&path, &transaction)?;
            recover_fact_transaction(project_root, &transaction)?;
            remove_fact_transaction(project_root, &transaction.patch_id)?;
            Ok(transaction.patch_id)
        })();
        match result {
            Ok(patch_id) => report.recovered.push(patch_id),
            Err(error) => report.failures.push(FactRecoveryFailure {
                path: path.display().to_string(),
                patch_id: path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(ToString::to_string),
                reason: error.to_string(),
            }),
        }
    }
    persist_fact_recovery_report(project_root, &report)?;
    Ok(report)
}

pub fn latest_fact_recovery_report(project_root: &Path) -> Result<Option<FactRecoveryReport>> {
    let path = fact_recovery_report_path(project_root);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
}

fn persist_fact_recovery_report(project_root: &Path, report: &FactRecoveryReport) -> Result<()> {
    if report.recovered.is_empty() && report.failures.is_empty() {
        return Ok(());
    }
    let unchanged = latest_fact_recovery_report(project_root)
        .ok()
        .flatten()
        .is_some_and(|previous| {
            previous.recovered == report.recovered && previous.failures == report.failures
        });
    if unchanged {
        return Ok(());
    }
    atomic_write(
        &fact_recovery_report_path(project_root),
        serde_json::to_string_pretty(report)?.as_bytes(),
    )?;
    if !report.recovered.is_empty() {
        record_event(
            project_root,
            AgentEventType::FactPatchRecoveryCompleted,
            "完成 Fact Patch 事务恢复",
            serde_json::json!({"patch_ids":report.recovered,"count":report.recovered.len()}),
        )?;
    }
    if !report.failures.is_empty() {
        record_event(
            project_root,
            AgentEventType::FactPatchRecoveryBlocked,
            "Fact Patch 事务恢复受阻",
            serde_json::json!({
                "count":report.failures.len(),
                "failures":report.failures.iter().map(|failure| serde_json::json!({
                    "patch_id":failure.patch_id,
                    "path":failure.path,
                    "reason":failure.reason
                })).collect::<Vec<_>>()
            }),
        )?;
    }
    Ok(())
}

fn ensure_recovery_clean(report: FactRecoveryReport) -> Result<()> {
    if let Some(failure) = report.failures.first() {
        anyhow::bail!(
            "存在无法安全恢复的事实事务 {}：{}",
            failure.patch_id.as_deref().unwrap_or(&failure.path),
            failure.reason
        );
    }
    Ok(())
}

fn recover_fact_transaction(project_root: &Path, transaction: &FactPatchTransaction) -> Result<()> {
    let mut patch = read_fact_patch(project_root, &transaction.patch_id)?;
    let fingerprints_match = match transaction.operation {
        FactTransactionOperation::Apply => {
            transaction.before_fingerprint == facts_fingerprint(&patch.before)
                && transaction.after_fingerprint == facts_fingerprint(&patch.after)
        }
        FactTransactionOperation::Revert => {
            transaction.before_fingerprint == facts_fingerprint(&patch.after)
                && transaction.after_fingerprint == facts_fingerprint(&patch.before)
        }
    };
    if !fingerprints_match {
        anyhow::bail!(
            "事实事务 {} 与 Patch 快照指纹不一致，拒绝恢复",
            transaction.patch_id
        );
    }
    let mut facts = list_facts(project_root)?;
    match transaction.operation {
        FactTransactionOperation::Apply => {
            if !matches!(
                patch.status,
                FactPatchStatus::Pending | FactPatchStatus::Applying | FactPatchStatus::Applied
            ) {
                anyhow::bail!("事实事务 {} 的应用状态无效", transaction.patch_id);
            }
            if snapshot_matches(&facts, &patch.after, &patch.before) {
                finalize_applied_patch(project_root, &mut patch, true)?;
            } else if snapshot_matches(&facts, &patch.before, &patch.after) {
                replace_facts(&mut facts, &patch.after);
                write_facts(project_root, &facts)?;
                finalize_applied_patch(project_root, &mut patch, true)?;
            } else {
                anyhow::bail!(
                    "事实事务 {} 无法安全恢复：账本既不匹配应用前快照，也不匹配应用后快照",
                    transaction.patch_id
                );
            }
        }
        FactTransactionOperation::Revert => {
            if !matches!(
                patch.status,
                FactPatchStatus::Applied | FactPatchStatus::Reverting | FactPatchStatus::Reverted
            ) {
                anyhow::bail!("事实事务 {} 的撤销状态无效", transaction.patch_id);
            }
            if snapshot_matches(&facts, &patch.before, &patch.after) {
                finalize_reverted_patch(project_root, &mut patch, true)?;
            } else if snapshot_matches(&facts, &patch.after, &patch.before) {
                restore_before_snapshot(&mut facts, &patch);
                write_facts(project_root, &facts)?;
                finalize_reverted_patch(project_root, &mut patch, true)?;
            } else {
                anyhow::bail!(
                    "事实事务 {} 无法安全恢复：账本既不匹配撤销前快照，也不匹配撤销后快照",
                    transaction.patch_id
                );
            }
        }
    }
    Ok(())
}

/// 只读检查待恢复事务，不修改 Fact Ledger、Patch 或事务日志。
pub fn diagnose_fact_patch_transactions(project_root: &Path) -> Result<FactTransactionDiagnostics> {
    let dir = fact_transactions_dir(project_root);
    if !dir.exists() {
        return Ok(FactTransactionDiagnostics::default());
    }
    let mut diagnostics = FactTransactionDiagnostics::default();
    let mut paths = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        diagnostics.pending_count += 1;
        let diagnostic = diagnose_fact_transaction(project_root, &path);
        if matches!(
            diagnostic.status,
            FactTransactionDiagnosticStatus::RecoverableBefore
                | FactTransactionDiagnosticStatus::RecoverableAfter
        ) {
            diagnostics.recoverable_count += 1;
        } else {
            diagnostics.blocked_count += 1;
        }
        diagnostics.transactions.push(diagnostic);
    }
    Ok(diagnostics)
}

fn diagnose_fact_transaction(project_root: &Path, path: &Path) -> FactTransactionDiagnostic {
    let mut diagnostic = FactTransactionDiagnostic {
        patch_id: path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(ToString::to_string),
        path: path.display().to_string(),
        operation: None,
        status: FactTransactionDiagnosticStatus::Invalid,
        reason: String::new(),
    };
    let result = (|| -> Result<(FactTransactionDiagnosticStatus, String)> {
        let transaction = serde_json::from_str::<FactPatchTransaction>(&fs::read_to_string(path)?)?;
        diagnostic.patch_id = Some(transaction.patch_id.clone());
        diagnostic.operation = Some(
            match transaction.operation {
                FactTransactionOperation::Apply => "apply",
                FactTransactionOperation::Revert => "revert",
            }
            .to_string(),
        );
        validate_transaction_path(path, &transaction)?;
        let patch = read_fact_patch(project_root, &transaction.patch_id)?;
        let facts = list_facts(project_root)?;
        let (before, after) = match transaction.operation {
            FactTransactionOperation::Apply => (&patch.before, &patch.after),
            FactTransactionOperation::Revert => (&patch.after, &patch.before),
        };
        if transaction.before_fingerprint != facts_fingerprint(before)
            || transaction.after_fingerprint != facts_fingerprint(after)
        {
            anyhow::bail!("事务与 Patch 快照指纹不一致");
        }
        if snapshot_matches(&facts, before, after) {
            Ok((
                FactTransactionDiagnosticStatus::RecoverableBefore,
                "账本匹配操作前快照，可安全继续事务".to_string(),
            ))
        } else if snapshot_matches(&facts, after, before) {
            Ok((
                FactTransactionDiagnosticStatus::RecoverableAfter,
                "账本匹配操作后快照，可安全完成事务".to_string(),
            ))
        } else {
            Ok((
                FactTransactionDiagnosticStatus::Blocked,
                "账本既不匹配操作前快照，也不匹配操作后快照".to_string(),
            ))
        }
    })();
    match result {
        Ok((status, reason)) => {
            diagnostic.status = status;
            diagnostic.reason = reason;
        }
        Err(error) => diagnostic.reason = error.to_string(),
    }
    diagnostic
}

pub fn compile_fact_context(
    project_root: &Path,
    query: &str,
    budget_tokens: usize,
    limit: usize,
) -> Result<FactContext> {
    let query_tokens = tokens(query);
    let mut scored = list_facts(project_root)?
        .into_iter()
        .filter(|fact| fact.status == FactStatus::Active)
        .filter_map(|fact| {
            let mut fact_tokens = tokens(&fact.statement);
            for evidence in &fact.evidence {
                fact_tokens.extend(tokens(evidence));
            }
            let overlap = query_tokens.intersection(&fact_tokens).count() as u32;
            let exact = if !query.trim().is_empty()
                && fact
                    .statement
                    .to_lowercase()
                    .contains(&query.to_lowercase())
            {
                10
            } else {
                0
            };
            let failure_bonus = if fact.fact_type == FactType::FailedApproach {
                2
            } else {
                0
            };
            let score = exact + overlap * 3 + failure_bonus;
            (score > 0).then_some(FactContextItem {
                fact,
                relevance_score: score,
                relevance_reason: format!("关键词重合 {} 项", overlap),
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .relevance_score
            .cmp(&left.relevance_score)
            .then_with(|| right.fact.updated_at.cmp(&left.fact.updated_at))
    });
    scored.truncate(limit.clamp(1, 100));

    let budget = budget_tokens.clamp(256, 16_000);
    let mut selected = Vec::new();
    let mut estimated_tokens = 0;
    let mut truncated = false;
    for item in scored {
        let tokens = estimate_tokens(&item.fact.statement)
            + item
                .fact
                .evidence
                .iter()
                .map(|value| estimate_tokens(value))
                .sum::<usize>();
        if estimated_tokens + tokens > budget {
            truncated = true;
            continue;
        }
        estimated_tokens += tokens;
        selected.push(item);
    }
    Ok(FactContext {
        query: query.to_string(),
        facts: selected,
        estimated_tokens,
        budget_tokens: budget,
        truncated,
    })
}

pub fn reconcile_knowledge(
    project_root: &Path,
    task_id: Option<String>,
) -> Result<ReconciliationReport> {
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let facts = list_facts(project_root)?
        .into_iter()
        .filter(|fact| fact.status == FactStatus::Active)
        .take(1_000)
        .collect::<Vec<_>>();
    let mut findings = Vec::new();
    let mut seen_pairs = HashSet::new();

    for (index, left) in facts.iter().enumerate() {
        for right in facts.iter().skip(index + 1) {
            let pair = if left.id < right.id {
                (left.id.clone(), right.id.clone())
            } else {
                (right.id.clone(), left.id.clone())
            };
            if !seen_pairs.insert(pair) {
                continue;
            }
            let similarity = token_similarity(&left.statement, &right.statement);
            if similarity >= 0.65 {
                findings.push(finding(
                    ReconciliationKind::Duplicate,
                    vec![left.id.clone(), right.id.clone()],
                    format!("两条事实语义高度重复，相似度 {:.0}%", similarity * 100.0),
                    "merge",
                    (similarity * 100.0) as u8,
                ));
            } else if similarity >= 0.35 && polarity(&left.statement) != polarity(&right.statement)
            {
                findings.push(finding(
                    ReconciliationKind::Conflict,
                    vec![left.id.clone(), right.id.clone()],
                    "两条相关事实包含相反约束，需要确认哪条仍然有效".to_string(),
                    "supersede",
                    80,
                ));
            }
        }
    }

    for fact in &facts {
        let verification = verify_fact(project_root, fact);
        let expired = fact
            .valid_until
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value < Utc::now());
        let all_evidence_missing = !verification.results.is_empty()
            && verification
                .results
                .iter()
                .all(|result| result.status == EvidenceVerificationStatus::Missing);
        let has_evidence_drift = verification.results.iter().any(|result| {
            matches!(
                result.status,
                EvidenceVerificationStatus::HashMismatch
                    | EvidenceVerificationStatus::InvalidLocation
            )
        });
        if expired {
            findings.push(finding(
                ReconciliationKind::Stale,
                vec![fact.id.clone()],
                "事实已经超过有效期限".to_string(),
                "delete",
                95,
            ));
        } else if all_evidence_missing {
            let reasons = verification
                .results
                .iter()
                .map(|result| result.reason.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join("；");
            findings.push(finding(
                ReconciliationKind::Stale,
                vec![fact.id.clone()],
                format!("事实的结构化证据均已消失：{}", reasons),
                "delete",
                90,
            ));
        } else if has_evidence_drift {
            let reasons = verification
                .results
                .iter()
                .filter(|result| {
                    matches!(
                        result.status,
                        EvidenceVerificationStatus::HashMismatch
                            | EvidenceVerificationStatus::InvalidLocation
                    )
                })
                .map(|result| result.reason.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join("；");
            findings.push(finding(
                ReconciliationKind::EvidenceDrift,
                vec![fact.id.clone()],
                format!("事实证据发生漂移，需要更新或重新验证：{}", reasons),
                "update",
                80,
            ));
        }
    }

    findings.sort_by_key(|item| std::cmp::Reverse(item.confidence));
    let now = Utc::now().to_rfc3339();
    let report = ReconciliationReport {
        id: item_id("reconcile", task_id.as_deref().unwrap_or("project"), &now),
        task_id,
        duplicate_count: findings
            .iter()
            .filter(|value| value.kind == ReconciliationKind::Duplicate)
            .count(),
        conflict_count: findings
            .iter()
            .filter(|value| value.kind == ReconciliationKind::Conflict)
            .count(),
        stale_count: findings
            .iter()
            .filter(|value| value.kind == ReconciliationKind::Stale)
            .count(),
        drift_count: findings
            .iter()
            .filter(|value| value.kind == ReconciliationKind::EvidenceDrift)
            .count(),
        findings,
        created_at: now,
    };
    let path = reconciliation_dir(project_root).join(format!("{}.json", report.id));
    fs::write(path, serde_json::to_string_pretty(&report)?)?;
    record_event(
        project_root,
        AgentEventType::KnowledgeReconciled,
        "完成项目知识对账",
        serde_json::json!({
            "report_id":report.id,
            "duplicates":report.duplicate_count,
            "conflicts":report.conflict_count,
            "stale":report.stale_count,
            "drift":report.drift_count
        }),
    )?;
    Ok(report)
}

pub fn latest_reconciliation(project_root: &Path) -> Result<Option<ReconciliationReport>> {
    let dir = reconciliation_dir(project_root);
    if !dir.exists() {
        return Ok(None);
    }
    let reports = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .map(|path| {
            Ok(serde_json::from_str::<ReconciliationReport>(
                &fs::read_to_string(path)?,
            )?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(reports.into_iter().max_by_key(|report| {
        DateTime::parse_from_rfc3339(&report.created_at)
            .map(|value| value.timestamp_millis())
            .unwrap_or(i64::MIN)
    }))
}

fn upsert_fact(project_root: &Path, mut fact: ProjectFact) -> Result<ProjectFact> {
    let now = Utc::now().to_rfc3339();
    prepare_fact(project_root, &mut fact, &now);
    let mut facts = list_facts(project_root)?;
    if let Some(existing) = facts
        .iter_mut()
        .find(|value| value.statement == fact.statement && value.status == FactStatus::Active)
    {
        existing.evidence.extend(fact.evidence);
        existing.evidence = dedupe_strings(std::mem::take(&mut existing.evidence));
        existing.evidence_details.extend(fact.evidence_details);
        existing.evidence_details = dedupe_evidence(std::mem::take(&mut existing.evidence_details));
        existing.confidence = existing.confidence.max(fact.confidence);
        existing.updated_at = Utc::now().to_rfc3339();
        existing.last_verified_at = existing.updated_at.clone();
        let result = existing.clone();
        write_facts(project_root, &facts)?;
        return Ok(result);
    }
    facts.push(fact.clone());
    write_facts(project_root, &facts)?;
    Ok(fact)
}

fn write_facts(project_root: &Path, facts: &[ProjectFact]) -> Result<()> {
    let content = facts
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n");
    atomic_write(
        &facts_path(project_root),
        format!("{}\n", content).as_bytes(),
    )
}

fn normalize_fact(fact: &mut ProjectFact, now: &str) {
    fact.evidence = dedupe_strings(std::mem::take(&mut fact.evidence));
    if fact.created_at.is_empty() {
        fact.created_at = now.to_string();
    }
    fact.updated_at = now.to_string();
    if fact.last_verified_at.is_empty() {
        fact.last_verified_at = now.to_string();
    }
    if fact.evidence_details.is_empty() {
        fact.evidence_details = fact
            .evidence
            .iter()
            .map(|path| FactEvidence {
                path: path.clone(),
                symbol: None,
                line_start: None,
                line_end: None,
                content_hash: None,
                hash_scope: None,
                git_head: None,
                captured_at: now.to_string(),
                verified_at: now.to_string(),
                evidence_type: "file".to_string(),
            })
            .collect();
    } else {
        fact.evidence.extend(
            fact.evidence_details
                .iter()
                .map(|evidence| evidence.path.clone()),
        );
        fact.evidence = dedupe_strings(std::mem::take(&mut fact.evidence));
    }
}

fn prepare_fact(project_root: &Path, fact: &mut ProjectFact, now: &str) {
    normalize_fact(fact, now);
    let git_head = current_git_head(project_root);
    for evidence in &mut fact.evidence_details {
        if evidence.captured_at.is_empty() {
            evidence.captured_at = now.to_string();
        }
        if evidence.verified_at.is_empty() {
            evidence.verified_at = now.to_string();
        }
        if evidence.git_head.is_none() {
            evidence.git_head = git_head.clone();
        }
        if evidence.content_hash.is_none()
            && let Ok(path) = resolve_evidence_path(project_root, &evidence.path)
            && fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_EVIDENCE_FILE_BYTES)
            && let Ok(hash) = hash_evidence_content(&path, evidence, None)
        {
            evidence.content_hash = Some(hash);
        }
    }
}

fn find_fact(facts: &[ProjectFact], id: &str) -> Result<ProjectFact> {
    validate_fact_identifier(id)?;
    facts
        .iter()
        .find(|fact| fact.id == id)
        .cloned()
        .with_context(|| format!("未找到事实: {}", id))
}

fn ensure_active_fact(fact: &ProjectFact, operation: &str) -> Result<()> {
    if fact.status != FactStatus::Active {
        anyhow::bail!(
            "{} 只允许作用于 Active Fact，当前状态为 {:?}: {}",
            operation,
            fact.status,
            fact.id
        );
    }
    Ok(())
}

fn facts_for_ids<'a>(
    facts: &[ProjectFact],
    ids: impl Iterator<Item = &'a str>,
) -> Result<Vec<ProjectFact>> {
    ids.map(|id| find_fact(facts, id)).collect()
}

fn replace_facts(facts: &mut Vec<ProjectFact>, replacements: &[ProjectFact]) {
    for replacement in replacements {
        if let Some(existing) = facts.iter_mut().find(|fact| fact.id == replacement.id) {
            *existing = replacement.clone();
        } else {
            facts.push(replacement.clone());
        }
    }
}

fn facts_fingerprint(facts: &[ProjectFact]) -> String {
    let mut facts = facts.to_vec();
    facts.sort_by(|left, right| left.id.cmp(&right.id));
    format!(
        "{:016x}",
        digest(&serde_json::to_string(&facts).unwrap_or_default())
    )
}

fn fact_patches_dir(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("fact-patches")
}

fn fact_transactions_dir(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("fact-transactions")
}

fn fact_recovery_report_path(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("latest-fact-recovery.json")
}

fn fact_patch_path(project_root: &Path, patch_id: &str) -> PathBuf {
    fact_patches_dir(project_root).join(format!("{}.json", patch_id))
}

fn write_fact_patch(project_root: &Path, patch: &FactPatch) -> Result<()> {
    validate_patch_identifier(&patch.id)?;
    fs::create_dir_all(fact_patches_dir(project_root))?;
    atomic_write(
        &fact_patch_path(project_root, &patch.id),
        serde_json::to_string_pretty(patch)?.as_bytes(),
    )
}

fn read_fact_patch(project_root: &Path, patch_id: &str) -> Result<FactPatch> {
    validate_patch_identifier(patch_id)?;
    let path = fact_patch_path(project_root, patch_id);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取事实草稿: {}", path.display()))?;
    let patch = serde_json::from_str::<FactPatch>(&content)?;
    if patch.id != patch_id {
        anyhow::bail!("事实草稿文件名与内部 Patch ID 不一致");
    }
    Ok(patch)
}

fn fact_transaction_path(project_root: &Path, patch_id: &str) -> PathBuf {
    fact_transactions_dir(project_root).join(format!("{}.json", patch_id))
}

fn write_fact_transaction(project_root: &Path, transaction: &FactPatchTransaction) -> Result<()> {
    validate_patch_identifier(&transaction.patch_id)?;
    atomic_write(
        &fact_transaction_path(project_root, &transaction.patch_id),
        serde_json::to_string_pretty(transaction)?.as_bytes(),
    )
}

fn remove_fact_transaction(project_root: &Path, patch_id: &str) -> Result<()> {
    validate_patch_identifier(patch_id)?;
    let path = fact_transaction_path(project_root, patch_id);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("无法移除已完成的事实事务日志: {}", path.display()))?;
    }
    Ok(())
}

fn validate_transaction_path(path: &Path, transaction: &FactPatchTransaction) -> Result<()> {
    validate_patch_identifier(&transaction.patch_id)?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("事实事务文件名不是有效 UTF-8")?;
    if stem != transaction.patch_id {
        anyhow::bail!("事实事务文件名与内部 Patch ID 不一致");
    }
    Ok(())
}

fn validate_fact_identifier(value: &str) -> Result<()> {
    validate_identifier(value, None, "Fact")
}

fn validate_new_fact_identifier(value: &str) -> Result<()> {
    validate_identifier(value, Some("fact_"), "Fact")
}

fn validate_patch_identifier(value: &str) -> Result<()> {
    validate_identifier(value, Some("fact_patch_"), "Fact Patch")
}

fn validate_identifier(value: &str, prefix: Option<&str>, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        anyhow::bail!(
            "{} ID 长度必须在 1 到 {} 字节之间",
            label,
            MAX_IDENTIFIER_BYTES
        );
    }
    if prefix.is_some_and(|prefix| !value.starts_with(prefix)) {
        anyhow::bail!("{} ID 必须以 {} 开头", label, prefix.unwrap_or_default());
    }
    if !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        anyhow::bail!("{} ID 只能包含 ASCII 字母、数字、下划线和连字符", label);
    }
    Ok(())
}

fn finalize_applied_patch(
    project_root: &Path,
    patch: &mut FactPatch,
    recovered: bool,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    patch.applied_fingerprint = Some(facts_fingerprint(&patch.after));
    patch.status = FactPatchStatus::Applied;
    patch.applied_at.get_or_insert_with(|| now.clone());
    patch.audit_events.push(FactPatchAuditEvent {
        action: if recovered {
            "apply_recovered"
        } else {
            "applied"
        }
        .to_string(),
        created_at: now,
        detail: if recovered {
            "已从事务日志恢复并完成事实治理草稿应用"
        } else {
            "已应用事实治理草稿"
        }
        .to_string(),
    });
    write_fact_patch(project_root, patch)
}

fn finalize_reverted_patch(
    project_root: &Path,
    patch: &mut FactPatch,
    recovered: bool,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    patch.status = FactPatchStatus::Reverted;
    patch.reverted_at.get_or_insert_with(|| now.clone());
    patch.audit_events.push(FactPatchAuditEvent {
        action: if recovered {
            "revert_recovered"
        } else {
            "reverted"
        }
        .to_string(),
        created_at: now,
        detail: if recovered {
            "已从事务日志恢复并完成事实治理草稿撤销"
        } else {
            "已撤销事实治理草稿"
        }
        .to_string(),
    });
    write_fact_patch(project_root, patch)
}

fn snapshot_matches(
    facts: &[ProjectFact],
    expected: &[ProjectFact],
    counterpart: &[ProjectFact],
) -> bool {
    let expected_ids = expected
        .iter()
        .map(|fact| fact.id.as_str())
        .collect::<HashSet<_>>();
    let expected_matches = expected.iter().all(|expected_fact| {
        facts.iter().find(|fact| fact.id == expected_fact.id) == Some(expected_fact)
    });
    let counterpart_only_absent = counterpart
        .iter()
        .filter(|fact| !expected_ids.contains(fact.id.as_str()))
        .all(|fact| !facts.iter().any(|current| current.id == fact.id));
    expected_matches && counterpart_only_absent
}

fn restore_before_snapshot(facts: &mut Vec<ProjectFact>, patch: &FactPatch) {
    replace_facts(facts, &patch.before);
    let before_ids = patch
        .before
        .iter()
        .map(|fact| fact.id.as_str())
        .collect::<HashSet<_>>();
    facts.retain(|fact| {
        !patch
            .after
            .iter()
            .any(|after| after.id == fact.id && !before_ids.contains(after.id.as_str()))
    });
}

fn finding(
    kind: ReconciliationKind,
    fact_ids: Vec<String>,
    reason: String,
    operation: &str,
    confidence: u8,
) -> ReconciliationFinding {
    let seed = format!("{:?}:{}", kind, fact_ids.join(":"));
    ReconciliationFinding {
        id: item_id("finding", &seed, operation),
        kind,
        fact_ids,
        reason,
        recommended_operation: operation.to_string(),
        confidence,
    }
}

fn resolve_task_id_for_session(
    project_root: &Path,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<String> {
    if let Some(id) = task_id {
        return Ok(id.to_string());
    }
    read_active_task_id_for_session(project_root, session_id)?
        .context("当前没有活动任务，请先调用 begin_task")
}

fn push_task_activity(task: &mut TaskRecord, kind: &str, summary: String, created_at: String) {
    const HOT_WINDOW: usize = 32;
    task.recent_activity.push(TaskActivity {
        kind: kind.to_string(),
        summary,
        created_at,
    });
    if task.recent_activity.len() > HOT_WINDOW {
        let retired = task.recent_activity.remove(0);
        let item = format!("[{}] {}", retired.kind, retired.summary);
        if task.phase_summary.is_empty() {
            task.phase_summary = item;
        } else {
            task.phase_summary.push('；');
            task.phase_summary.push_str(&item);
        }
        const SUMMARY_LIMIT: usize = 4_000;
        if task.phase_summary.chars().count() > SUMMARY_LIMIT {
            task.phase_summary = task
                .phase_summary
                .chars()
                .rev()
                .take(SUMMARY_LIMIT)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
    }
}

fn ensure_active(task: &TaskRecord) -> Result<()> {
    if task.status != TaskStatus::Active {
        anyhow::bail!("任务已经关闭: {}", task.id);
    }
    Ok(())
}

fn ensure_task_session(task: &TaskRecord, session_id: Option<&str>) -> Result<()> {
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty())
        && task.session_id.as_deref() != Some(session_id)
    {
        anyhow::bail!("任务不属于当前 session: {}", session_id);
    }
    Ok(())
}

fn read_active_task_id_for_session(
    project_root: &Path,
    session_id: Option<&str>,
) -> Result<Option<String>> {
    let path = active_task_path_for_session(project_root, session_id);
    if !path.exists() {
        return Ok(None);
    }
    let id = fs::read_to_string(path)?.trim().to_string();
    Ok((!id.is_empty()).then_some(id))
}

fn write_active_task_id(
    project_root: &Path,
    session_id: Option<&str>,
    task_id: &str,
) -> Result<()> {
    let path = active_task_path_for_session(project_root, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, task_id)?;
    Ok(())
}

fn read_task(project_root: &Path, task_id: &str) -> Result<TaskRecord> {
    let path = task_path(project_root, task_id);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取任务记录: {}", path.display()))?;
    Ok(serde_json::from_str(&content)?)
}

fn write_task(project_root: &Path, task: &TaskRecord) -> Result<()> {
    fs::write(
        task_path(project_root, &task.id),
        serde_json::to_string_pretty(task)?,
    )?;
    Ok(())
}

fn ensure_memory_dirs(project_root: &Path) -> Result<()> {
    fs::create_dir_all(tasks_dir(project_root))?;
    fs::create_dir_all(memory_dir(project_root))?;
    fs::create_dir_all(reconciliation_dir(project_root))?;
    fs::create_dir_all(fact_patches_dir(project_root))?;
    fs::create_dir_all(fact_transactions_dir(project_root))?;
    Ok(())
}

fn tasks_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("tasks")
}

fn task_path(project_root: &Path, task_id: &str) -> PathBuf {
    tasks_dir(project_root).join(format!("{}.json", task_id))
}

fn active_task_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("active-task")
}

fn active_task_path_for_session(project_root: &Path, session_id: Option<&str>) -> PathBuf {
    match session_id.filter(|value| !value.trim().is_empty()) {
        Some(session_id) => project_root
            .join(CYCLE_DIR)
            .join("active-tasks")
            .join(format!("{}.txt", safe_identifier(session_id))),
        None => active_task_path(project_root),
    }
}

fn safe_identifier(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect()
}

fn memory_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("memory")
}

fn facts_path(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("facts.jsonl")
}

fn reconciliation_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("reconciliation")
}

fn item_id(prefix: &str, value: &str, salt: &str) -> String {
    format!("{}_{:016x}", prefix, digest(&(value, salt)))
}

fn digest<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn dedupe_evidence(values: Vec<FactEvidence>) -> Vec<FactEvidence> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|evidence| {
            seen.insert(serde_json::to_string(evidence).unwrap_or_else(|_| evidence.path.clone()))
        })
        .collect()
}

fn tokens(value: &str) -> BTreeSet<String> {
    let lower = value.to_lowercase();
    let mut result = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|value| value.len() >= 2)
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let chinese = lower
        .chars()
        .filter(|value| ('\u{4e00}'..='\u{9fff}').contains(value))
        .collect::<Vec<_>>();
    for pair in chinese.windows(2) {
        result.insert(pair.iter().collect());
    }
    result
}

fn token_similarity(left: &str, right: &str) -> f32 {
    let left = tokens(left);
    let right = tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f32;
    let union = left.union(&right).count() as f32;
    intersection / union
}

fn polarity(value: &str) -> bool {
    [
        "不", "禁止", "不得", "不能", "无需", "never", "not", "forbid",
    ]
    .iter()
    .any(|marker| value.to_lowercase().contains(marker))
}

fn resolve_evidence_path(project_root: &Path, value: &str) -> Result<PathBuf> {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let candidate = PathBuf::from(value);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        if candidate
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            anyhow::bail!("证据相对路径不能包含 ..");
        }
        root.join(candidate)
    };
    let resolved = candidate.canonicalize().unwrap_or(candidate);
    if !resolved.starts_with(&root) {
        anyhow::bail!("证据路径超出项目根目录");
    }
    Ok(resolved)
}

fn hash_evidence_content(
    path: &Path,
    evidence: &FactEvidence,
    content: Option<&[u8]>,
) -> Result<String> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        anyhow::bail!("证据文件超过哈希大小上限");
    }
    let mut hasher = Sha256::new();
    match evidence
        .hash_scope
        .as_ref()
        .unwrap_or(&EvidenceHashScope::File)
    {
        EvidenceHashScope::File => {
            let file = fs::File::open(path)?;
            let mut reader = BufReader::new(file);
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        }
        EvidenceHashScope::LineRange => {
            let owned;
            let content = match content {
                Some(content) => content,
                None => {
                    owned = fs::read(path)?;
                    &owned
                }
            };
            let text = String::from_utf8_lossy(content);
            let start = evidence
                .line_start
                .context("line_range 哈希必须提供 line_start")?;
            let end = evidence.line_end.unwrap_or(start);
            if start == 0 || end < start {
                anyhow::bail!("证据行范围无效");
            }
            let selected = text
                .lines()
                .skip(start.saturating_sub(1) as usize)
                .take((end - start + 1) as usize)
                .collect::<Vec<_>>();
            if selected.len() != (end - start + 1) as usize {
                anyhow::bail!("证据行范围超出文件长度");
            }
            hasher.update(selected.join("\n").as_bytes());
        }
        EvidenceHashScope::Symbol => {
            let owned;
            let content = match content {
                Some(content) => content,
                None => {
                    owned = fs::read(path)?;
                    &owned
                }
            };
            let symbol = evidence
                .symbol
                .as_deref()
                .context("symbol 哈希必须提供 symbol")?;
            let text = String::from_utf8_lossy(content);
            let line = text
                .lines()
                .find(|line| line.contains(symbol))
                .context("证据符号已无法定位")?;
            hasher.update(line.as_bytes());
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn current_git_head(project_root: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn valid_evidence_location(content: &str, evidence: &FactEvidence) -> bool {
    let lines = content.lines().collect::<Vec<_>>();
    if let Some(start) = evidence.line_start {
        let end = evidence.line_end.unwrap_or(start);
        if start == 0 || end < start || end as usize > lines.len() {
            return false;
        }
    }
    evidence
        .symbol
        .as_deref()
        .is_none_or(|symbol| content.contains(symbol))
}

fn normalize_hash(value: &str) -> &str {
    value.strip_prefix("sha256:").unwrap_or(value)
}

fn evidence_verifications_path(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("evidence-verifications.jsonl")
}

fn evidence_verifications_archive_dir(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("evidence-verifications")
}

fn evidence_verification_archive_index_path(archive_path: &Path) -> PathBuf {
    archive_path.with_extension("index.json")
}

fn evidence_verification_paths(project_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = evidence_verifications_archive_dir(project_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn write_evidence_verification_archive_index(
    source_path: &Path,
    archive_path: &Path,
) -> Result<PathBuf> {
    let index_path = evidence_verification_archive_index_path(archive_path);
    if archive_path.exists() || index_path.exists() {
        anyhow::bail!("证据验证归档名称冲突: {}", archive_path.display());
    }
    let source_metadata = fs::metadata(source_path)?;
    let source_file = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("证据验证归档缺少合法文件名")?
        .to_string();
    let mut reader = BufReader::new(File::open(source_path)?);
    let mut entries = Vec::new();
    let mut offset = 0_u64;
    let mut line_number = 0_usize;
    loop {
        let mut line = Vec::new();
        let length = reader.read_until(b'\n', &mut line)?;
        if length == 0 {
            break;
        }
        line_number += 1;
        let record_bytes = line.strip_suffix(b"\n").unwrap_or(&line);
        let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
        if !record_bytes.iter().all(u8::is_ascii_whitespace) {
            let record = serde_json::from_slice::<EvidenceVerificationRecord>(record_bytes)
                .with_context(|| {
                    format!(
                        "证据验证账本损坏: {}:{}",
                        source_path.display(),
                        line_number
                    )
                })?;
            entries.push(EvidenceVerificationArchiveEntry {
                fact_id: record.fact_id,
                checked_at: record.checked_at,
                offset,
                length: length as u64,
            });
        }
        offset = offset.saturating_add(length as u64);
    }
    let index = EvidenceVerificationArchiveIndex {
        schema_version: 1,
        source_file,
        source_size: source_metadata.len(),
        source_modified_ns: metadata_modified_ns(&source_metadata),
        entries,
    };
    atomic_write(
        &index_path,
        serde_json::to_string_pretty(&index)?.as_bytes(),
    )?;
    Ok(index_path)
}

fn read_evidence_verification_archive_index(
    archive_path: &Path,
) -> Result<Option<EvidenceVerificationArchiveIndex>> {
    let index_path = evidence_verification_archive_index_path(archive_path);
    if !index_path.exists() {
        return Ok(None);
    }
    let Ok(content) = fs::read_to_string(&index_path) else {
        return Ok(None);
    };
    let Ok(index) = serde_json::from_str::<EvidenceVerificationArchiveIndex>(&content) else {
        return Ok(None);
    };
    let metadata = fs::metadata(archive_path)?;
    let expected_file = archive_path.file_name().and_then(|value| value.to_str());
    let entries_valid = index.entries.iter().all(|entry| {
        entry.length > 0
            && entry
                .offset
                .checked_add(entry.length)
                .is_some_and(|end| end <= metadata.len())
    });
    if index.schema_version != 1
        || expected_file != Some(index.source_file.as_str())
        || index.source_size != metadata.len()
        || index.source_modified_ns != metadata_modified_ns(&metadata)
        || !entries_valid
    {
        return Ok(None);
    }
    Ok(Some(index))
}

fn read_indexed_evidence_verification(
    archive_path: &Path,
    entry: &EvidenceVerificationArchiveEntry,
) -> Result<EvidenceVerificationRecord> {
    let mut file = File::open(archive_path)?;
    file.seek(std::io::SeekFrom::Start(entry.offset))?;
    let length = usize::try_from(entry.length).context("证据验证归档索引长度超出平台限制")?;
    let mut line = vec![0_u8; length];
    file.read_exact(&mut line)?;
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    let record =
        serde_json::from_slice::<EvidenceVerificationRecord>(&line).with_context(|| {
            format!(
                "证据验证归档索引指向无效记录: {}:{}",
                archive_path.display(),
                entry.offset
            )
        })?;
    if record.fact_id != entry.fact_id || record.checked_at != entry.checked_at {
        anyhow::bail!(
            "证据验证归档索引与账本不一致: {}:{}",
            archive_path.display(),
            entry.offset
        );
    }
    Ok(record)
}

fn metadata_modified_ns(metadata: &fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn append_evidence_verification(
    project_root: &Path,
    report: &FactVerificationReport,
) -> Result<()> {
    append_evidence_verification_with_limit(
        project_root,
        report,
        EVIDENCE_VERIFICATION_ROLL_BYTES,
        EVIDENCE_VERIFICATION_ARCHIVE_LIMIT,
    )
}

fn append_evidence_verification_with_limit(
    project_root: &Path,
    report: &FactVerificationReport,
    roll_bytes: u64,
    archive_limit: usize,
) -> Result<()> {
    let path = evidence_verifications_path(project_root);
    repair_evidence_verification_tail(&path)?;
    let record = EvidenceVerificationRecord {
        id: item_id("evidence_verification", &report.fact_id, &report.checked_at),
        fact_id: report.fact_id.clone(),
        git_head: current_git_head(project_root),
        results: report.results.clone(),
        verified_count: report.verified_count,
        issue_count: report.issue_count,
        checked_at: report.checked_at.clone(),
    };
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    let current_len = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
    if current_len > 0 && current_len.saturating_add(line.len() as u64) > roll_bytes.max(1) {
        let archive_dir = evidence_verifications_archive_dir(project_root);
        fs::create_dir_all(&archive_dir)?;
        let archive_name = format!(
            "evidence-verifications-{}.jsonl",
            Utc::now().format("%Y%m%dT%H%M%S%fZ")
        );
        let archive_path = archive_dir.join(archive_name);
        let index_path = write_evidence_verification_archive_index(&path, &archive_path)?;
        if let Err(error) = fs::rename(&path, &archive_path) {
            let _ = fs::remove_file(index_path);
            return Err(error).with_context(|| {
                format!(
                    "无法滚动证据验证账本: {} -> {}",
                    path.display(),
                    archive_path.display()
                )
            });
        }
        prune_evidence_verification_archives(project_root, archive_limit)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&line)?;
    file.sync_all()?;
    Ok(())
}

fn repair_evidence_verification_tail(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read(path)?;
    if content.is_empty() || content.ends_with(b"\n") {
        return Ok(());
    }
    let tail_start = content
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let tail = &content[tail_start..];
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    if serde_json::from_slice::<EvidenceVerificationRecord>(tail).is_ok() {
        file.seek(std::io::SeekFrom::End(0))?;
        file.write_all(b"\n")?;
    } else {
        file.set_len(tail_start as u64)?;
    }
    file.sync_all()?;
    Ok(())
}

fn prune_evidence_verification_archives(project_root: &Path, limit: usize) -> Result<()> {
    let paths = evidence_verification_paths(project_root)?;
    let remove_count = paths.len().saturating_sub(limit);
    for path in paths.into_iter().take(remove_count) {
        fs::remove_file(&path)?;
        let index_path = evidence_verification_archive_index_path(&path);
        if index_path.exists() {
            fs::remove_file(index_path)?;
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().context("原子写入目标缺少父目录")?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("无法创建同目录临时文件: {}", parent.display()))?;
    temporary.write_all(content)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换文件: {}", path.display()))?;
    Ok(())
}

fn estimate_tokens(value: &str) -> usize {
    value.chars().count().div_ceil(3).max(1)
}

fn record_event(
    project_root: &Path,
    event_type: AgentEventType,
    summary: &str,
    data: serde_json::Value,
) -> Result<()> {
    append_event(
        project_root,
        &new_event(event_type, "cyclaw-memory", summary, data),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: &str, statement: &str) -> ProjectFact {
        let now = "2026-01-01T00:00:00Z".to_string();
        ProjectFact {
            id: id.to_string(),
            statement: statement.to_string(),
            fact_type: FactType::Constraint,
            status: FactStatus::Active,
            evidence: vec!["src/lib.rs".to_string()],
            evidence_details: Vec::new(),
            source_task_id: None,
            confidence: 90,
            valid_from: None,
            valid_until: None,
            supersedes: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_verified_at: now,
        }
    }

    fn seed(temp: &tempfile::TempDir, facts: &[ProjectFact]) {
        ensure_memory_dirs(temp.path()).unwrap();
        write_facts(temp.path(), facts).unwrap();
    }

    #[test]
    fn task_memory_survives_close_and_new_context() {
        let temp = tempfile::tempdir().unwrap();
        let task = begin_task(BeginTaskOptions::new(
            temp.path().to_path_buf(),
            "退款状态机".to_string(),
            "修改退款状态流转".to_string(),
        ))
        .unwrap();
        record_decision(
            temp.path(),
            Some(&task.id),
            "退款完成后不得重新进入处理中".to_string(),
            "避免重复退款".to_string(),
            vec!["src/refund.rs".to_string()],
            95,
        )
        .unwrap();
        close_task(temp.path(), Some(&task.id), "完成状态机修改".to_string()).unwrap();

        let context = compile_fact_context(temp.path(), "退款处理中", 500, 10).unwrap();
        assert_eq!(context.facts.len(), 1);
        assert!(context.facts[0].fact.statement.contains("不得重新"));
    }

    #[test]
    fn session_tasks_have_isolated_active_pointers() {
        let temp = tempfile::tempdir().unwrap();
        let first = begin_task(BeginTaskOptions {
            session_id: Some("session-a".to_string()),
            ..BeginTaskOptions::new(temp.path().to_path_buf(), "A".to_string(), "A".to_string())
        })
        .unwrap();
        let second = begin_task(BeginTaskOptions {
            session_id: Some("session-b".to_string()),
            ..BeginTaskOptions::new(temp.path().to_path_buf(), "B".to_string(), "B".to_string())
        })
        .unwrap();

        assert_eq!(
            get_active_task_for_session(temp.path(), Some("session-a"))
                .unwrap()
                .unwrap()
                .id,
            first.id
        );
        assert_eq!(
            get_active_task_for_session(temp.path(), Some("session-b"))
                .unwrap()
                .unwrap()
                .id,
            second.id
        );
    }

    #[test]
    fn task_activity_uses_bounded_hot_window() {
        let temp = tempfile::tempdir().unwrap();
        let task = begin_task(BeginTaskOptions::new(
            temp.path().to_path_buf(),
            "长任务".to_string(),
            "验证滑动窗口".to_string(),
        ))
        .unwrap();
        for index in 0..35 {
            checkpoint_task(
                temp.path(),
                Some(&task.id),
                format!("检查点 {}", index),
                Vec::new(),
            )
            .unwrap();
        }
        let saved = get_task(temp.path(), &task.id).unwrap();
        assert_eq!(saved.recent_activity.len(), 32);
        assert!(saved.phase_summary.contains("检查点 0"));
    }

    #[test]
    fn reconciliation_finds_duplicate_and_stale_facts() {
        let temp = tempfile::tempdir().unwrap();
        let task = begin_task(BeginTaskOptions::new(
            temp.path().to_path_buf(),
            "依赖治理".to_string(),
            "统一包管理器".to_string(),
        ))
        .unwrap();
        record_decision(
            temp.path(),
            Some(&task.id),
            "项目统一使用 pnpm 11 作为包管理器".to_string(),
            "保证锁文件一致".to_string(),
            vec!["missing/package.json".to_string()],
            90,
        )
        .unwrap();
        record_decision(
            temp.path(),
            Some(&task.id),
            "项目统一使用 pnpm 11 作为包管理器和依赖工具".to_string(),
            "避免 npm 混用".to_string(),
            vec!["missing/pnpm-lock.yaml".to_string()],
            90,
        )
        .unwrap();

        let report = reconcile_knowledge(temp.path(), Some(task.id)).unwrap();
        assert!(report.stale_count >= 2);
        assert!(report.duplicate_count >= 1);
    }

    #[test]
    fn fact_patch_create_update_and_delete_are_reversible() {
        let temp = tempfile::tempdir().unwrap();
        let created = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Create,
                target_fact_id: None,
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "只允许 pnpm")),
            },
        )
        .unwrap();
        let created = apply_fact_patch(temp.path(), &created.id).unwrap();
        let id = created.after[0].id.clone();
        let updated = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some(id.clone()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("ignored", "只允许 pnpm 11")),
            },
        )
        .unwrap();
        apply_fact_patch(temp.path(), &updated.id).unwrap();
        assert_eq!(
            list_facts(temp.path()).unwrap()[0].statement,
            "只允许 pnpm 11"
        );
        let deleted = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Delete,
                target_fact_id: Some(id),
                source_fact_ids: Vec::new(),
                fact: None,
            },
        )
        .unwrap();
        let deleted = apply_fact_patch(temp.path(), &deleted.id).unwrap();
        assert_eq!(
            list_facts(temp.path()).unwrap()[0].status,
            FactStatus::Deleted
        );
        revert_fact_patch(temp.path(), &deleted.id).unwrap();
        assert_eq!(
            list_facts(temp.path()).unwrap()[0].status,
            FactStatus::Active
        );
    }

    #[test]
    fn fact_patch_merge_and_supersede_preserve_history() {
        let temp = tempfile::tempdir().unwrap();
        seed(
            &temp,
            &[fact("one", "使用 pnpm"), fact("two", "项目使用 pnpm")],
        );
        let merged = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Merge,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: vec!["two".to_string()],
                fact: None,
            },
        )
        .unwrap();
        apply_fact_patch(temp.path(), &merged.id).unwrap();
        let facts = list_facts(temp.path()).unwrap();
        assert_eq!(
            facts.iter().find(|fact| fact.id == "two").unwrap().status,
            FactStatus::Superseded
        );
        let superseded = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Supersede,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("fact_three", "使用 pnpm 11")),
            },
        )
        .unwrap();
        apply_fact_patch(temp.path(), &superseded.id).unwrap();
        let facts = list_facts(temp.path()).unwrap();
        assert_eq!(
            facts.iter().find(|fact| fact.id == "one").unwrap().status,
            FactStatus::Superseded
        );
        assert!(
            facts
                .iter()
                .find(|fact| fact.id == "fact_three")
                .unwrap()
                .supersedes
                .contains(&"one".to_string())
        );
    }

    #[test]
    fn apply_rejects_concurrent_fact_change_after_preview() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        let mut changed = list_facts(temp.path()).unwrap();
        changed[0].statement = "外部修改".to_string();
        write_facts(temp.path(), &changed).unwrap();
        assert!(
            apply_fact_patch(temp.path(), &patch.id)
                .unwrap_err()
                .to_string()
                .contains("预览后已变化")
        );
    }

    #[test]
    fn revert_rejects_concurrent_fact_change_after_apply() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        apply_fact_patch(temp.path(), &patch.id).unwrap();
        let mut changed = list_facts(temp.path()).unwrap();
        changed[0].statement = "外部修改".to_string();
        write_facts(temp.path(), &changed).unwrap();
        assert!(
            revert_fact_patch(temp.path(), &patch.id)
                .unwrap_err()
                .to_string()
                .contains("应用后已变化")
        );
    }

    #[test]
    fn verifies_structured_evidence_and_detects_hash_drift() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        let path = temp.path().join("src/lib.rs");
        fs::write(&path, "pub fn governed() {}\n").unwrap();
        let hash = format!("sha256:{:x}", Sha256::digest(fs::read(&path).unwrap()));
        let mut item = fact("one", "事实证据必须可验证");
        item.evidence_details = vec![FactEvidence {
            path: "src/lib.rs".to_string(),
            symbol: Some("governed".to_string()),
            line_start: Some(1),
            line_end: Some(1),
            content_hash: Some(hash),
            hash_scope: None,
            git_head: Some("abc123".to_string()),
            captured_at: item.created_at.clone(),
            verified_at: item.last_verified_at.clone(),
            evidence_type: "source".to_string(),
        }];
        seed(&temp, &[item]);

        let verified = verify_fact_evidence(temp.path(), "one").unwrap();
        assert_eq!(verified.verified_count, 1);
        assert_eq!(verified.issue_count, 0);

        fs::write(path, "pub fn governed() { println!(\"changed\"); }\n").unwrap();
        let drifted = verify_fact_evidence(temp.path(), "one").unwrap();
        assert_eq!(
            drifted.results[0].status,
            EvidenceVerificationStatus::HashMismatch
        );
        let reconciliation = reconcile_knowledge(temp.path(), None).unwrap();
        assert_eq!(reconciliation.stale_count, 0);
        assert_eq!(reconciliation.drift_count, 1);
        assert_eq!(reconciliation.findings[0].recommended_operation, "update");
    }

    #[test]
    fn expired_fact_takes_precedence_over_evidence_drift() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("evidence.txt"), "当前内容").unwrap();
        let mut item = fact("one", "已经到期的事实");
        item.valid_until = Some("2020-01-01T00:00:00Z".to_string());
        item.evidence_details = vec![FactEvidence {
            path: "evidence.txt".to_string(),
            symbol: None,
            line_start: None,
            line_end: None,
            content_hash: Some("sha256:outdated".to_string()),
            hash_scope: Some(EvidenceHashScope::File),
            git_head: None,
            captured_at: item.created_at.clone(),
            verified_at: item.last_verified_at.clone(),
            evidence_type: "file".to_string(),
        }];
        seed(&temp, &[item]);

        let reconciliation = reconcile_knowledge(temp.path(), None).unwrap();
        assert_eq!(reconciliation.stale_count, 1);
        assert_eq!(reconciliation.drift_count, 0);
        assert_eq!(reconciliation.findings[0].recommended_operation, "delete");
        assert!(reconciliation.findings[0].reason.contains("有效期限"));
    }

    #[test]
    fn evidence_verification_rejects_paths_outside_project() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let mut item = fact("one", "证据不能越过项目边界");
        item.evidence_details = vec![FactEvidence {
            path: outside.path().display().to_string(),
            symbol: None,
            line_start: None,
            line_end: None,
            content_hash: None,
            hash_scope: None,
            git_head: None,
            captured_at: item.created_at.clone(),
            verified_at: item.last_verified_at.clone(),
            evidence_type: "file".to_string(),
        }];
        seed(&temp, &[item]);

        let report = verify_fact_evidence(temp.path(), "one").unwrap();
        assert_eq!(
            report.results[0].status,
            EvidenceVerificationStatus::OutsideProject
        );
        let reconciliation = reconcile_knowledge(temp.path(), None).unwrap();
        assert_eq!(reconciliation.stale_count, 0);
        assert_eq!(reconciliation.drift_count, 0);
    }

    #[test]
    fn fact_files_are_atomically_replaced_without_temporary_residue() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "第一版")]);
        write_facts(temp.path(), &[fact("one", "第二版")]).unwrap();

        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "第二版");
        let unexpected = fs::read_dir(memory_dir(temp.path()))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
            .count();
        assert_eq!(unexpected, 0);
    }

    #[test]
    fn governance_rejects_changes_to_non_active_fact() {
        let temp = tempfile::tempdir().unwrap();
        let mut deleted = fact("one", "已经删除");
        deleted.status = FactStatus::Deleted;
        seed(&temp, &[deleted]);

        let error = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("one", "试图更新")),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("Active Fact"));
    }

    #[test]
    fn recovers_interrupted_apply_from_transaction_log() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        let transaction = FactPatchTransaction {
            patch_id: patch.id.clone(),
            operation: FactTransactionOperation::Apply,
            started_at: Utc::now().to_rfc3339(),
            before_fingerprint: facts_fingerprint(&patch.before),
            after_fingerprint: facts_fingerprint(&patch.after),
        };
        write_fact_transaction(temp.path(), &transaction).unwrap();
        let mut interrupted = patch.clone();
        interrupted.status = FactPatchStatus::Applying;
        write_fact_patch(temp.path(), &interrupted).unwrap();

        let recovered = recover_fact_patch_transactions(temp.path()).unwrap();

        assert_eq!(recovered.recovered, vec![patch.id.clone()]);
        assert!(recovered.failures.is_empty());
        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "新事实");
        assert_eq!(
            read_fact_patch(temp.path(), &patch.id).unwrap().status,
            FactPatchStatus::Applied
        );
        assert!(!fact_transaction_path(temp.path(), &patch.id).exists());
        let latest = latest_fact_recovery_report(temp.path()).unwrap().unwrap();
        assert_eq!(latest.recovered, vec![patch.id.clone()]);
        assert!(latest.failures.is_empty());
        let events = cyclaw_events::read_events(temp.path()).unwrap();
        assert!(events.iter().any(|event| {
            event.event_type == AgentEventType::FactPatchRecoveryCompleted
                && event.data["patch_ids"][0] == patch.id
        }));
    }

    #[test]
    fn recovery_rejects_ambiguous_concurrent_ledger_change() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        write_fact_transaction(
            temp.path(),
            &FactPatchTransaction {
                patch_id: patch.id.clone(),
                operation: FactTransactionOperation::Apply,
                started_at: Utc::now().to_rfc3339(),
                before_fingerprint: facts_fingerprint(&patch.before),
                after_fingerprint: facts_fingerprint(&patch.after),
            },
        )
        .unwrap();
        write_facts(temp.path(), &[fact("one", "并发修改")]).unwrap();

        let report = recover_fact_patch_transactions(temp.path()).unwrap();

        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].reason.contains("无法安全恢复"));
        assert!(fact_transaction_path(temp.path(), &patch.id).exists());
        let event_count = cyclaw_events::read_events(temp.path())
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == AgentEventType::FactPatchRecoveryBlocked)
            .count();
        assert_eq!(event_count, 1);

        let repeated = recover_fact_patch_transactions(temp.path()).unwrap();
        assert_eq!(repeated.failures.len(), 1);
        let repeated_event_count = cyclaw_events::read_events(temp.path())
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == AgentEventType::FactPatchRecoveryBlocked)
            .count();
        assert_eq!(repeated_event_count, 1);
    }

    #[test]
    fn recovers_interrupted_revert_after_ledger_was_restored() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        let mut patch = apply_fact_patch(temp.path(), &patch.id).unwrap();
        write_fact_transaction(
            temp.path(),
            &FactPatchTransaction {
                patch_id: patch.id.clone(),
                operation: FactTransactionOperation::Revert,
                started_at: Utc::now().to_rfc3339(),
                before_fingerprint: facts_fingerprint(&patch.after),
                after_fingerprint: facts_fingerprint(&patch.before),
            },
        )
        .unwrap();
        patch.status = FactPatchStatus::Reverting;
        write_fact_patch(temp.path(), &patch).unwrap();
        write_facts(temp.path(), &patch.before).unwrap();

        recover_fact_patch_transactions(temp.path()).unwrap();

        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "旧事实");
        assert_eq!(
            read_fact_patch(temp.path(), &patch.id).unwrap().status,
            FactPatchStatus::Reverted
        );
    }

    #[test]
    fn verification_ledger_preserves_history_without_mutating_fact() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("evidence.txt"),
            "第一行\n受治理内容\n第三行\n",
        )
        .unwrap();
        let mut item = fact("one", "行范围证据只跟踪目标片段");
        item.evidence_details = vec![FactEvidence {
            path: "evidence.txt".to_string(),
            symbol: None,
            line_start: Some(2),
            line_end: Some(2),
            content_hash: None,
            hash_scope: Some(EvidenceHashScope::LineRange),
            git_head: None,
            captured_at: String::new(),
            verified_at: String::new(),
            evidence_type: "file".to_string(),
        }];
        prepare_fact(temp.path(), &mut item, &Utc::now().to_rfc3339());
        let original = item.clone();
        seed(&temp, &[item]);

        verify_fact_evidence(temp.path(), "one").unwrap();
        fs::write(
            temp.path().join("evidence.txt"),
            "已改第一行\n受治理内容\n第三行\n",
        )
        .unwrap();
        let unchanged_scope = verify_fact_evidence(temp.path(), "one").unwrap();
        assert_eq!(unchanged_scope.verified_count, 1);
        fs::write(
            temp.path().join("evidence.txt"),
            "已改第一行\n目标已变化\n第三行\n",
        )
        .unwrap();
        let changed_scope = verify_fact_evidence(temp.path(), "one").unwrap();

        assert_eq!(
            changed_scope.results[0].status,
            EvidenceVerificationStatus::HashMismatch
        );
        assert_eq!(list_facts(temp.path()).unwrap()[0], original);
        assert_eq!(
            list_evidence_verifications(temp.path(), Some("one"), 0, 10)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn evidence_verification_rejects_oversized_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.bin");
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_EVIDENCE_FILE_BYTES + 1)
            .unwrap();
        let mut item = fact("one", "大文件证据受大小限制");
        item.evidence_details = vec![FactEvidence {
            path: "large.bin".to_string(),
            symbol: None,
            line_start: None,
            line_end: None,
            content_hash: None,
            hash_scope: Some(EvidenceHashScope::File),
            git_head: None,
            captured_at: item.created_at.clone(),
            verified_at: item.last_verified_at.clone(),
            evidence_type: "file".to_string(),
        }];
        seed(&temp, &[item]);

        let report = verify_fact_evidence(temp.path(), "one").unwrap();

        assert_eq!(
            report.results[0].status,
            EvidenceVerificationStatus::TooLarge
        );
    }

    #[test]
    fn fact_patch_identifiers_reject_path_traversal_and_invalid_values() {
        let temp = tempfile::tempdir().unwrap();
        for invalid in [
            "../../outside",
            "..\\outside",
            "fact_patch_/../../x",
            "C:\\outside",
            "fact_patch_bad\nvalue",
            "wrong_prefix",
        ] {
            let error = apply_fact_patch(temp.path(), invalid).unwrap_err();
            assert!(error.to_string().contains("ID"), "未拒绝: {invalid}");
        }
        let too_long = format!("fact_patch_{}", "a".repeat(MAX_IDENTIFIER_BYTES));
        assert!(apply_fact_patch(temp.path(), &too_long).is_err());
    }

    #[test]
    fn transaction_filename_must_match_internal_patch_id() {
        let temp = tempfile::tempdir().unwrap();
        ensure_memory_dirs(temp.path()).unwrap();
        let transaction = FactPatchTransaction {
            patch_id: "fact_patch_internal".to_string(),
            operation: FactTransactionOperation::Apply,
            started_at: Utc::now().to_rfc3339(),
            before_fingerprint: "before".to_string(),
            after_fingerprint: "after".to_string(),
        };
        fs::write(
            fact_transactions_dir(temp.path()).join("fact_patch_filename.json"),
            serde_json::to_string(&transaction).unwrap(),
        )
        .unwrap();

        let report = recover_fact_patch_transactions(temp.path()).unwrap();
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].reason.contains("文件名"));
        let diagnostics = diagnose_fact_patch_transactions(temp.path()).unwrap();
        assert_eq!(diagnostics.blocked_count, 1);
    }

    #[test]
    fn recovery_isolates_bad_transaction_and_recovers_valid_one() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        write_fact_transaction(
            temp.path(),
            &FactPatchTransaction {
                patch_id: patch.id.clone(),
                operation: FactTransactionOperation::Apply,
                started_at: Utc::now().to_rfc3339(),
                before_fingerprint: facts_fingerprint(&patch.before),
                after_fingerprint: facts_fingerprint(&patch.after),
            },
        )
        .unwrap();
        fs::write(
            fact_transactions_dir(temp.path()).join("fact_patch_broken.json"),
            "{broken",
        )
        .unwrap();

        let report = recover_fact_patch_transactions(temp.path()).unwrap();
        assert_eq!(report.recovered, vec![patch.id]);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "新事实");
    }

    #[test]
    fn evidence_verification_ledger_rolls_and_reads_archives() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "需要验证")]);
        for index in 0..5 {
            let report = FactVerificationReport {
                fact_id: "one".to_string(),
                results: Vec::new(),
                verified_count: 0,
                issue_count: 0,
                checked_at: format!("2026-08-04T12:00:0{index}Z"),
            };
            append_evidence_verification_with_limit(temp.path(), &report, 1, 2).unwrap();
        }

        let archives = evidence_verification_paths(temp.path()).unwrap();
        assert_eq!(archives.len(), 2);
        for archive in &archives {
            let index_path = evidence_verification_archive_index_path(archive);
            assert!(index_path.exists());
            let index = read_evidence_verification_archive_index(archive)
                .unwrap()
                .unwrap();
            assert_eq!(index.schema_version, 1);
            assert_eq!(index.entries.len(), 1);
        }
        let index_count = fs::read_dir(evidence_verifications_archive_dir(temp.path()))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".index.json"))
            .count();
        assert_eq!(index_count, 2);
        let records = list_evidence_verifications(temp.path(), Some("one"), 0, 20).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].checked_at, "2026-08-04T12:00:04Z");
        let page = query_evidence_verifications(temp.path(), Some("one"), 1, 1).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.records[0].checked_at, "2026-08-04T12:00:03Z");

        fs::write(
            evidence_verification_archive_index_path(&archives[0]),
            [0xff, 0xfe],
        )
        .unwrap();
        let fallback = query_evidence_verifications(temp.path(), Some("one"), 0, 20).unwrap();
        assert_eq!(fallback.records.len(), 3);
    }

    #[test]
    fn stale_evidence_verification_archive_index_falls_back_to_strict_parsing() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "需要验证")]);
        for index in 0..2 {
            let report = FactVerificationReport {
                fact_id: "one".to_string(),
                results: Vec::new(),
                verified_count: 0,
                issue_count: 0,
                checked_at: format!("2026-08-04T12:00:0{index}Z"),
            };
            append_evidence_verification_with_limit(temp.path(), &report, 1, 2).unwrap();
        }
        let archive = evidence_verification_paths(temp.path()).unwrap().remove(0);
        OpenOptions::new()
            .append(true)
            .open(&archive)
            .unwrap()
            .write_all(b"{broken}\n")
            .unwrap();

        let error = query_evidence_verifications(temp.path(), None, 0, 20).unwrap_err();
        assert!(error.to_string().contains("账本损坏"));
    }

    #[test]
    fn evidence_verification_ledger_recovers_incomplete_tail() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "需要验证")]);
        verify_fact_evidence(temp.path(), "one").unwrap();
        let path = evidence_verifications_path(temp.path());
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"partial\":")
            .unwrap();

        let damaged = query_evidence_verifications(temp.path(), Some("one"), 0, 20).unwrap();
        assert_eq!(damaged.records.len(), 1);
        assert_eq!(damaged.issues.len(), 1);
        assert_eq!(damaged.issues[0].line, 2);

        verify_fact_evidence(temp.path(), "one").unwrap();
        let repaired = query_evidence_verifications(temp.path(), Some("one"), 0, 20).unwrap();
        assert_eq!(repaired.records.len(), 2);
        assert!(repaired.issues.is_empty());
    }

    #[test]
    fn evidence_verification_archive_rejects_corrupted_complete_line() {
        let temp = tempfile::tempdir().unwrap();
        ensure_memory_dirs(temp.path()).unwrap();
        let archive_dir = evidence_verifications_archive_dir(temp.path());
        fs::create_dir_all(&archive_dir).unwrap();
        fs::write(
            archive_dir.join("evidence-verifications-20200101.jsonl"),
            "{broken}\n",
        )
        .unwrap();

        let error = query_evidence_verifications(temp.path(), None, 0, 20).unwrap_err();
        assert!(error.to_string().contains("账本损坏"));
        assert!(error.to_string().contains(":1:"));
    }

    #[test]
    fn concurrent_apply_allows_only_one_writer() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        let root = Arc::new(temp.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                let patch_id = patch.id.clone();
                thread::spawn(move || {
                    barrier.wait();
                    apply_fact_patch(&root, &patch_id)
                })
            })
            .collect::<Vec<_>>();
        let success_count = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(Result::is_ok)
            .count();

        assert_eq!(success_count, 1);
        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "新事实");
    }

    #[test]
    fn concurrent_evidence_verification_does_not_drop_records() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "并发验证")]);
        let root = Arc::new(temp.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    verify_fact_evidence(&root, "one")
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let records = list_evidence_verifications(temp.path(), Some("one"), 0, 20).unwrap();
        assert_eq!(records.len(), 8);
    }

    #[test]
    fn transaction_recovery_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        seed(&temp, &[fact("one", "旧事实")]);
        let patch = preview_fact_patch(
            temp.path(),
            FactPatchRequest {
                operation: FactOperation::Update,
                target_fact_id: Some("one".to_string()),
                source_fact_ids: Vec::new(),
                fact: Some(fact("", "新事实")),
            },
        )
        .unwrap();
        write_fact_transaction(
            temp.path(),
            &FactPatchTransaction {
                patch_id: patch.id.clone(),
                operation: FactTransactionOperation::Apply,
                started_at: Utc::now().to_rfc3339(),
                before_fingerprint: facts_fingerprint(&patch.before),
                after_fingerprint: facts_fingerprint(&patch.after),
            },
        )
        .unwrap();

        let first = recover_fact_patch_transactions(temp.path()).unwrap();
        let second = recover_fact_patch_transactions(temp.path()).unwrap();
        assert_eq!(first.recovered, vec![patch.id]);
        assert!(second.recovered.is_empty());
        assert!(second.failures.is_empty());
        assert_eq!(list_facts(temp.path()).unwrap()[0].statement, "新事实");
    }

    #[test]
    fn latest_reconciliation_uses_created_at_instead_of_random_id() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(reconciliation_dir(temp.path())).unwrap();
        let report = |id: &str, created_at: &str| ReconciliationReport {
            id: id.to_string(),
            task_id: None,
            findings: Vec::new(),
            duplicate_count: 0,
            conflict_count: 0,
            stale_count: 0,
            drift_count: 0,
            created_at: created_at.to_string(),
        };
        let older = report("z_older", "2026-08-04T12:00:00Z");
        let newer = report("a_newer", "2026-08-04T21:01:00+09:00");
        fs::write(
            reconciliation_dir(temp.path()).join("z_older.json"),
            serde_json::to_string(&older).unwrap(),
        )
        .unwrap();
        fs::write(
            reconciliation_dir(temp.path()).join("a_newer.json"),
            serde_json::to_string(&newer).unwrap(),
        )
        .unwrap();

        let latest = latest_reconciliation(temp.path()).unwrap().unwrap();

        assert_eq!(latest.id, "a_newer");
    }
}
