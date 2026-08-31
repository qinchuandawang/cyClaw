use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

const CYCLE_DIR: &str = ".cyclaw";
const EVENTS_FILE: &str = "events.jsonl";
const EXECUTION_EVENTS_FILE: &str = "execution-events.jsonl";
static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 进程内严格递增，并携带时间与进程信息，避免并发运行覆盖持久化记录。
pub fn new_id(prefix: &str) -> String {
    let now = Utc::now();
    let nanos = now.timestamp_nanos_opt().unwrap_or_default();
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{:x}-{:x}-{:x}",
        prefix,
        nanos,
        std::process::id(),
        sequence
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEvent {
    pub schema_version: u32,
    pub id: String,
    pub event_type: AgentEventType,
    pub created_at: String,
    pub source: String,
    pub summary: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventType {
    ProjectScanned,
    GitChanged,
    KnowledgeCandidateCreated,
    KnowledgeCandidateUpdated,
    DocumentPatchCreated,
    DocumentPatchApplied,
    ModelProviderConfigured,
    PolicyChanged,
    IndexUpdated,
    ModelCalled,
    AgentRunCompleted,
    HookRun,
    TaskStarted,
    TaskCheckpointed,
    TaskClosed,
    FactRecorded,
    FactPatchCreated,
    FactPatchApplied,
    FactPatchReverted,
    FactPatchRecoveryCompleted,
    FactPatchRecoveryBlocked,
    KnowledgeReconciled,
    ObserverStarted,
    ObserverReconciled,
    ObserverArtifactObserved,
    ExecutionObserved,
    ExecutionFailed,
    ExecutionSucceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionEvent {
    pub schema_version: u32,
    pub id: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub source: String,
    pub kind: ExecutionEventKind,
    pub command_summary: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub related_files: Vec<String>,
    pub error_summary: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub duration_millis: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEventKind {
    Command,
    Build,
    Test,
    Patch,
}

pub fn execution_events_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(EXECUTION_EVENTS_FILE)
}

pub fn append_execution_event(project_root: &Path, event: &ExecutionEvent) -> Result<PathBuf> {
    let path = execution_events_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建执行事件目录: {}", parent.display()))?;
    }
    let line = serde_json::to_string(event)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{}", line)?;
    Ok(path)
}

pub fn read_execution_events(project_root: &Path) -> Result<Vec<ExecutionEvent>> {
    let path = execution_events_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect::<Vec<ExecutionEvent>>())
}

pub fn new_execution_event(
    source: impl Into<String>,
    kind: ExecutionEventKind,
    command_summary: impl Into<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    related_files: Vec<String>,
    error_summary: Option<String>,
) -> ExecutionEvent {
    new_execution_event_with_context(
        source,
        kind,
        command_summary,
        exit_code,
        timed_out,
        related_files,
        error_summary,
        None,
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn new_execution_event_with_context(
    source: impl Into<String>,
    kind: ExecutionEventKind,
    command_summary: impl Into<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    related_files: Vec<String>,
    error_summary: Option<String>,
    session_id: Option<String>,
    trace_id: Option<String>,
    git_commit: Option<String>,
    duration_millis: Option<u128>,
) -> ExecutionEvent {
    let command_summary = command_summary.into();
    let id = new_id("execution");
    ExecutionEvent {
        schema_version: 1,
        idempotency_key: format!(
            "{}:{}:{}:{}:{:016x}",
            kind_name(&kind),
            command_summary,
            exit_code.unwrap_or_default(),
            timed_out,
            execution_digest(&related_files, error_summary.as_deref()),
        ),
        id,
        created_at: Utc::now().to_rfc3339(),
        source: source.into(),
        kind,
        command_summary,
        exit_code,
        timed_out,
        related_files,
        error_summary,
        session_id,
        trace_id,
        git_commit,
        duration_millis,
    }
}

fn execution_digest(related_files: &[String], error_summary: Option<&str>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    related_files.hash(&mut hasher);
    error_summary.unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

fn kind_name(kind: &ExecutionEventKind) -> &'static str {
    match kind {
        ExecutionEventKind::Command => "command",
        ExecutionEventKind::Build => "build",
        ExecutionEventKind::Test => "test",
        ExecutionEventKind::Patch => "patch",
    }
}

pub fn events_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(EVENTS_FILE)
}

pub fn append_event(project_root: &Path, event: &AgentEvent) -> Result<PathBuf> {
    let path = events_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建事件目录: {}", parent.display()))?;
    }

    let line = serde_json::to_string(event)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("无法打开事件日志: {}", path.display()))?;
    writeln!(file, "{}", line).with_context(|| format!("无法写入事件日志: {}", path.display()))?;
    Ok(path)
}

pub fn read_events(project_root: &Path) -> Result<Vec<AgentEvent>> {
    let path = events_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取事件日志: {}", path.display()))?;
    let mut events = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(event) = serde_json::from_str(line) {
            events.push(event);
        }
    }
    Ok(events)
}

pub fn new_event(
    event_type: AgentEventType,
    source: impl Into<String>,
    summary: impl Into<String>,
    data: serde_json::Value,
) -> AgentEvent {
    let now = Utc::now();
    AgentEvent {
        schema_version: 1,
        id: new_id("event"),
        event_type,
        created_at: now.to_rfc3339(),
        source: source.into(),
        summary: summary.into(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_and_reads_events() {
        let temp = tempfile::tempdir().unwrap();
        let event = new_event(
            AgentEventType::AgentRunCompleted,
            "test",
            "agent completed",
            json!({ "run_id": "agent-1" }),
        );

        let path = append_event(temp.path(), &event).unwrap();
        let events = read_events(temp.path()).unwrap();

        assert!(path.exists());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "agent completed");
    }

    #[test]
    fn execution_events_are_unique_and_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let first = new_execution_event(
            "test",
            ExecutionEventKind::Test,
            "cargo test",
            Some(1),
            false,
            vec!["src/lib.rs".to_string()],
            Some("断言失败".to_string()),
        );
        let second = new_execution_event(
            "test",
            ExecutionEventKind::Test,
            "cargo test",
            Some(1),
            false,
            Vec::new(),
            Some("断言失败".to_string()),
        );
        assert_ne!(first.id, second.id);
        append_execution_event(temp.path(), &first).unwrap();
        let events = read_execution_events(temp.path()).unwrap();
        assert_eq!(events[0].command_summary, "cargo test");
    }
}
