use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const CYCLE_DIR: &str = ".cyclaw";
const EVENTS_FILE: &str = "events.jsonl";
const EXECUTION_EVENTS_FILE: &str = "execution-events.jsonl";
const TRACES_FILE: &str = "traces.jsonl";
const MODEL_CALLS_FILE: &str = "model-calls.jsonl";
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
    RetryTaskScheduled,
    RetryTaskCompleted,
    RetryTaskExhausted,
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

// ---------- 链路追踪（Trace） ----------

/// 一次完整链路中的一个 span。trace_id 由任务发起方生成并贯通所有子调用，
/// 与 execution event、model call 记录通过 trace_id / span_id 互相关联。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceSpan {
    pub schema_version: u32,
    pub trace_id: String,
    pub span_id: String,
    #[serde(default)]
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: TraceSpanKind,
    pub source: String,
    pub status: TraceSpanStatus,
    pub started_at: String,
    pub ended_at: String,
    #[serde(default)]
    pub duration_millis: u128,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceSpanKind {
    AgentRun,
    Exec,
    ModelInference,
    Mcp,
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceSpanStatus {
    Ok,
    Error,
    Timeout,
}

/// 一次大模型推理调用的明细记录，用于回放推理路径；正文仅保存脱敏后的预览。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCallRecord {
    pub schema_version: u32,
    pub id: String,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub span_id: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub phase: Option<String>,
    pub status: ModelCallStatus,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub latency_millis: u128,
    #[serde(default)]
    pub attempts: usize,
    #[serde(default)]
    pub prompt_preview: Option<String>,
    #[serde(default)]
    pub response_preview: Option<String>,
    #[serde(default)]
    pub error_summary: Option<String>,
    /// 非 None 表示本次调用来自从该 Provider 降级切换后的备用 Provider。
    #[serde(default)]
    pub fallback_from: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelCallStatus {
    Success,
    CacheHit,
    Error,
}

pub fn new_trace_id() -> String {
    new_id("trace")
}

// ---------- 失败重试队列（可靠性） ----------

const RETRY_QUEUE_FILE: &str = "retry-queue.json";
/// 重试退避基准：第 n 次重试延迟 = base * 2^(n-1)，不超过上限。
pub const RETRY_BASE_DELAY_SECONDS: i64 = 60;
/// 默认重试上限，耗尽后任务降级为人工处理。
pub const RETRY_MAX_ATTEMPTS: usize = 5;
/// 退避上限，避免无限增长的等待时间。
pub const RETRY_MAX_BACKOFF_SECONDS: i64 = 3600;

/// 一条跨进程持久的失败任务；恢复后按退避计划异步重试，耗尽后降级为人工处理。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryTask {
    pub schema_version: u32,
    pub id: String,
    pub kind: RetryKind,
    pub status: RetryStatus,
    /// 任务执行所需的上下文（如 provider、候选列表）。
    pub payload: serde_json::Value,
    pub attempt_count: usize,
    pub max_attempts: usize,
    #[serde(default)]
    pub next_retry_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub degraded: bool,
    #[serde(default)]
    pub degraded_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryKind {
    ModelReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryStatus {
    Pending,
    InProgress,
    Completed,
    Exhausted,
    Abandoned,
}

pub fn retry_queue_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(RETRY_QUEUE_FILE)
}

pub fn read_retry_queue(project_root: &Path) -> Result<Vec<RetryTask>> {
    let path = retry_queue_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取重试队列: {}", path.display()))?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

/// 整队列原子替换写入；调用方需持有 retry-queue 项目锁避免并发覆盖。
pub fn write_retry_queue(project_root: &Path, tasks: &[RetryTask]) -> Result<()> {
    let path = retry_queue_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建重试队列目录: {}", parent.display()))?;
    }
    let mut temporary = NamedTempFile::new_in(path.parent().context("重试队列缺少父目录")?)?;
    serde_json::to_writer_pretty(&mut temporary, &tasks)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换重试队列: {}", path.display()))
        .map(|_| ())
}

pub fn new_retry_task(
    kind: RetryKind,
    payload: serde_json::Value,
    trace_id: Option<String>,
) -> RetryTask {
    let now = Utc::now().to_rfc3339();
    RetryTask {
        schema_version: 1,
        id: new_id("retry"),
        kind,
        status: RetryStatus::Pending,
        payload,
        attempt_count: 0,
        max_attempts: RETRY_MAX_ATTEMPTS,
        next_retry_at: Some(now.clone()),
        last_error: None,
        trace_id,
        created_at: now.clone(),
        updated_at: now,
        completed_at: None,
        degraded: false,
        degraded_reason: None,
    }
}

/// 第 attempt 次重试（attempt 从 1 开始）前需要的退避秒数。
pub fn retry_backoff_seconds(attempt: usize, base_seconds: i64, cap_seconds: i64) -> i64 {
    let exponent = attempt.saturating_sub(1).min(31) as u32;
    base_seconds
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(cap_seconds)
        .max(0)
}

/// 把退避秒数换算为下一次重试的 RFC3339 时间戳。
pub fn next_retry_timestamp(delay_seconds: i64) -> String {
    (Utc::now() + chrono::Duration::seconds(delay_seconds)).to_rfc3339()
}

/// 统一的 RFC3339 时间戳入口，供不直接依赖 chrono 的 crate 复用。
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub fn new_span_id() -> String {
    new_id("span")
}

#[allow(clippy::too_many_arguments)]
pub fn new_trace_span(
    trace_id: impl Into<String>,
    span_id: impl Into<String>,
    parent_span_id: Option<String>,
    name: impl Into<String>,
    kind: TraceSpanKind,
    source: impl Into<String>,
    status: TraceSpanStatus,
    started_at: String,
    ended_at: String,
    attributes: serde_json::Value,
) -> TraceSpan {
    let duration_millis = match (
        chrono::DateTime::parse_from_rfc3339(&started_at),
        chrono::DateTime::parse_from_rfc3339(&ended_at),
    ) {
        (Ok(start), Ok(end)) => (end - start).num_milliseconds().max(0) as u128,
        _ => 0,
    };
    TraceSpan {
        schema_version: 1,
        trace_id: trace_id.into(),
        span_id: span_id.into(),
        parent_span_id,
        name: name.into(),
        kind,
        source: source.into(),
        status,
        started_at,
        ended_at,
        duration_millis,
        attributes,
    }
}

pub fn traces_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(TRACES_FILE)
}

pub fn append_trace_span(project_root: &Path, span: &TraceSpan) -> Result<PathBuf> {
    let path = traces_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建链路追踪目录: {}", parent.display()))?;
    }
    let line = serde_json::to_string(span)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("无法打开链路追踪日志: {}", path.display()))?;
    writeln!(file, "{}", line)
        .with_context(|| format!("无法写入链路追踪日志: {}", path.display()))?;
    Ok(path)
}

pub fn read_trace_spans(project_root: &Path) -> Result<Vec<TraceSpan>> {
    let path = traces_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取链路追踪日志: {}", path.display()))?;
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

pub fn model_calls_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(MODEL_CALLS_FILE)
}

pub fn append_model_call(project_root: &Path, record: &ModelCallRecord) -> Result<PathBuf> {
    let path = model_calls_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建模型调用日志目录: {}", parent.display()))?;
    }
    let line = serde_json::to_string(record)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("无法打开模型调用日志: {}", path.display()))?;
    writeln!(file, "{}", line)
        .with_context(|| format!("无法写入模型调用日志: {}", path.display()))?;
    Ok(path)
}

pub fn read_model_calls(project_root: &Path) -> Result<Vec<ModelCallRecord>> {
    let path = model_calls_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取模型调用日志: {}", path.display()))?;
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
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

    #[test]
    fn trace_spans_roundtrip_and_compute_duration() {
        let temp = tempfile::tempdir().unwrap();
        let span = new_trace_span(
            new_trace_id(),
            new_span_id(),
            None,
            "model_call",
            TraceSpanKind::ModelInference,
            "cyclaw-model",
            TraceSpanStatus::Ok,
            "2026-01-01T00:00:00Z".to_string(),
            "2026-01-01T00:00:02Z".to_string(),
            json!({ "provider": "deepseek" }),
        );
        assert_eq!(span.duration_millis, 2000);
        append_trace_span(temp.path(), &span).unwrap();

        let spans = read_trace_spans(temp.path()).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, TraceSpanKind::ModelInference);
        assert_eq!(spans[0].status, TraceSpanStatus::Ok);
    }

    #[test]
    fn model_call_records_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let record = ModelCallRecord {
            schema_version: 1,
            id: new_id("model_call"),
            trace_id: Some("trace-1".to_string()),
            span_id: Some("span-1".to_string()),
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            phase: Some("agent_review".to_string()),
            status: ModelCallStatus::Success,
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            latency_millis: 800,
            attempts: 1,
            prompt_preview: Some("审查候选知识".to_string()),
            response_preview: None,
            error_summary: None,
            fallback_from: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: "2026-01-01T00:00:01Z".to_string(),
        };
        append_model_call(temp.path(), &record).unwrap();

        let records = read_model_calls(temp.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, ModelCallStatus::Success);
        assert_eq!(records[0].trace_id.as_deref(), Some("trace-1"));
        // 旧版数据兼容：缺少可选字段时也能读取。
        let missing_optional: ModelCallRecord =
            serde_json::from_str(r#"{"schema_version":1,"id":"m1","provider":"p","model":"m","status":"error","input_tokens":0,"output_tokens":0,"total_tokens":0,"latency_millis":0,"started_at":"","finished_at":""}"#).unwrap();
        assert_eq!(missing_optional.status, ModelCallStatus::Error);
    }

    #[test]
    fn retry_queue_roundtrip_and_status_transitions() {
        let temp = tempfile::tempdir().unwrap();
        let mut task = new_retry_task(
            RetryKind::ModelReview,
            json!({ "provider": "deepseek", "candidate_ids": ["kc-1"] }),
            Some("agent-1".to_string()),
        );
        assert_eq!(task.status, RetryStatus::Pending);
        assert_eq!(task.attempt_count, 0);
        assert_eq!(task.max_attempts, RETRY_MAX_ATTEMPTS);
        assert!(task.next_retry_at.is_some());

        task.status = RetryStatus::Exhausted;
        task.degraded = true;
        task.degraded_reason = Some("重试耗尽，降级为人工处理".to_string());
        task.attempt_count = task.max_attempts;
        write_retry_queue(temp.path(), &[task]).unwrap();

        let queue = read_retry_queue(temp.path()).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].status, RetryStatus::Exhausted);
        assert!(queue[0].degraded);
        assert_eq!(queue[0].attempt_count, RETRY_MAX_ATTEMPTS);
    }

    #[test]
    fn retry_backoff_grows_exponentially_with_cap() {
        assert_eq!(retry_backoff_seconds(1, 60, 3600), 60);
        assert_eq!(retry_backoff_seconds(2, 60, 3600), 120);
        assert_eq!(retry_backoff_seconds(3, 60, 3600), 240);
        assert_eq!(retry_backoff_seconds(7, 60, 3600), 3600);
        assert_eq!(retry_backoff_seconds(20, 60, 3600), 3600);
        // 上限本身是边界
        assert_eq!(retry_backoff_seconds(1, 0, 3600), 0);
    }
}
