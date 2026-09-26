use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_core::{
    list_document_patches, list_inbox, project_status, update_candidate_review, watch_project_once,
};
use cyclaw_docs::DocumentPatchStatus;
use cyclaw_events::{
    AgentEventType, RetryKind, RetryStatus, TraceSpanKind, TraceSpanStatus, append_event,
    append_trace_span, new_event, new_id, new_retry_task, new_span_id, new_trace_span,
    next_retry_timestamp, read_retry_queue, retry_backoff_seconds, write_retry_queue,
};
use cyclaw_knowledge::KnowledgeStatus;
use cyclaw_model::{
    TestProviderOptions, TestProviderResult, TraceContext, list_providers, test_provider,
};
use cyclaw_policy::{acquire_lock, load_or_default};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct ModelReviewResponse {
    reviews: Vec<ModelCandidateReview>,
}

#[derive(Debug, Deserialize)]
struct ModelCandidateReview {
    candidate_id: String,
    recommendation: String,
    confidence: u8,
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct CriticResponse {
    verdicts: Vec<CriticVerdict>,
}

/// 怀疑者（critic）对审查者 keep 结论的逐条裁决。
#[derive(Debug, Deserialize)]
struct CriticVerdict {
    candidate_id: String,
    /// confirm | downgrade | reject
    verdict: String,
    confidence: u8,
    rationale: String,
}

const CYCLE_DIR: &str = ".cyclaw";

#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub project_root: PathBuf,
    pub provider: Option<String>,
    pub use_model: bool,
    /// 本次执行来自重试调度时对应的队列条目 ID；run 结束后回写其状态。
    pub retry_task_id: Option<String>,
}

impl AgentRunOptions {
    pub fn new(project_root: PathBuf, provider: Option<String>, use_model: bool) -> Self {
        Self {
            project_root,
            provider,
            use_model,
            retry_task_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub project_root: String,
    pub started_at: String,
    pub completed_at: String,
    pub changed: bool,
    pub steps: Vec<AgentStep>,
    pub pending_knowledge_count: usize,
    pub pending_patch_count: usize,
    pub model_provider: Option<String>,
    /// 仅保存脱敏摘要，避免把外部模型输出作为长期项目数据留存。
    pub model_response_summary: Option<String>,
    #[serde(default)]
    pub resumed: bool,
    /// 本次 run 的链路 ID，与 traces.jsonl 中的 trace_id 一致。
    #[serde(default)]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentRunState {
    pub schema_version: u32,
    pub run_id: String,
    pub phase: String,
    pub provider: Option<String>,
    pub pending_candidate_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub name: String,
    pub status: AgentStepStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepStatus {
    Completed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub record_path: PathBuf,
    pub record: AgentRunRecord,
}

pub fn list_agent_runs(project_root: &Path, limit: usize) -> Result<Vec<AgentRunRecord>> {
    ensure_directory(project_root)?;
    let dir = project_root.join(CYCLE_DIR).join("agent-runs");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.reverse();
    paths.truncate(limit.max(1));
    paths
        .into_iter()
        .map(|path| {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("无法读取 Agent 运行记录: {}", path.display()))?;
            Ok(serde_json::from_str(&content)?)
        })
        .collect()
}

pub fn cleanup_agent_runs(project_root: &Path, keep: usize) -> Result<usize> {
    ensure_directory(project_root)?;
    let dir = project_root.join(CYCLE_DIR).join("agent-runs");
    if !dir.exists() {
        return Ok(0);
    }
    let mut paths = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    let delete_count = paths.len().saturating_sub(keep);
    for path in paths.into_iter().take(delete_count) {
        fs::remove_file(path)?;
    }
    Ok(delete_count)
}

pub fn run_agent_once(options: AgentRunOptions) -> Result<AgentRunResult> {
    ensure_directory(&options.project_root)?;
    let _run_lock = acquire_lock(
        &options.project_root,
        "agent-run",
        std::time::Duration::from_secs(5),
    )?;
    let started_at = Utc::now();
    let state_path = run_state_path(&options.project_root);
    let previous_state = read_run_state(&state_path)?;
    let resumed = previous_state.is_some();
    let run_id = previous_state
        .as_ref()
        .map(|state| state.run_id.clone())
        .unwrap_or_else(|| new_id("agent"));
    // 链路追踪：run_id 兼作 trace_id，模型审查与模型调用作为子 span 挂在同一 trace 下。
    let trace_id = run_id.clone();
    let root_span_id = new_span_id();
    let run_started_at = started_at.to_rfc3339();
    let mut steps = Vec::new();

    let status = project_status(options.project_root.clone())?;
    steps.push(AgentStep {
        name: "project_status".to_string(),
        status: AgentStepStatus::Completed,
        detail: format!(
            "initialized={}, git_has_changes={}",
            status.initialized, status.git_has_changes
        ),
    });

    let mut changed = false;
    let mut auto_applied_patches = 0;
    if status.git_has_changes {
        let tick = watch_project_once(&options.project_root, None)?;
        changed = tick.changed;
        auto_applied_patches = tick.auto_applied_patches;
        let generated = tick
            .inbox_result
            .as_ref()
            .map(|result| result.generated.len())
            .unwrap_or(0);
        steps.push(AgentStep {
            name: "watch_once".to_string(),
            status: AgentStepStatus::Completed,
            detail: format!(
                "changed={}, generated_candidates={}, auto_applied_patches={}",
                changed, generated, auto_applied_patches
            ),
        });
    } else {
        steps.push(AgentStep {
            name: "watch_once".to_string(),
            status: AgentStepStatus::Skipped,
            detail: "未发现 Git 未提交变更".to_string(),
        });
    }

    let inbox = list_inbox(options.project_root.clone())?;
    let pending_candidates = inbox
        .candidates
        .into_iter()
        .filter(|candidate| candidate.status == KnowledgeStatus::Pending)
        .collect::<Vec<_>>();
    steps.push(AgentStep {
        name: "list_pending_knowledge".to_string(),
        status: AgentStepStatus::Completed,
        detail: format!("pending={}", pending_candidates.len()),
    });

    let patches = list_document_patches(options.project_root.clone())?;
    let pending_patches = patches
        .into_iter()
        .filter(|patch| patch.status == DocumentPatchStatus::Pending)
        .collect::<Vec<_>>();
    steps.push(AgentStep {
        name: "list_document_patches".to_string(),
        status: AgentStepStatus::Completed,
        detail: format!("pending={}", pending_patches.len()),
    });

    steps.push(AgentStep {
        name: "auto_document_management".to_string(),
        status: if auto_applied_patches > 0 {
            AgentStepStatus::Completed
        } else {
            AgentStepStatus::Skipped
        },
        detail: if auto_applied_patches > 0 {
            format!("watch 已自动应用 {} 个文档草稿", auto_applied_patches)
        } else {
            "默认仅生成候选和草稿，或本次没有可自动应用的文档".to_string()
        },
    });

    let mut model_provider = None;
    let mut model_response_summary = None;
    if options.use_model {
        match resolve_provider(&options.project_root, options.provider.clone()) {
            Ok(Some(provider_name)) => {
                write_run_state(
                    &state_path,
                    &AgentRunState {
                        schema_version: 1,
                        run_id: run_id.clone(),
                        phase: "model_review_pending".to_string(),
                        provider: Some(provider_name.clone()),
                        pending_candidate_ids: pending_candidates
                            .iter()
                            .map(|candidate| candidate.id.clone())
                            .collect(),
                        updated_at: Utc::now().to_rfc3339(),
                    },
                )?;
                let prompt = render_agent_prompt(&pending_candidates, &pending_patches);
                let model_span_started_at = Utc::now().to_rfc3339();
                let model_span_id = new_span_id();
                let model_result = test_provider(TestProviderOptions {
                    project_root: options.project_root.clone(),
                    name: provider_name.clone(),
                    prompt,
                    trace: Some(TraceContext {
                        trace_id: trace_id.clone(),
                        parent_span_id: Some(model_span_id.clone()),
                        phase: Some("agent_review".to_string()),
                    }),
                });
                let mut reviewed = 0;
                let mut review_error = None;
                let mut response_summary = None;
                let mut critic_stats: Option<(usize, usize, usize)> = None;
                let mut critic_error = None;
                match model_result {
                    Ok(result) => {
                        append_event(
                            &options.project_root,
                            &new_event(
                                AgentEventType::ModelCalled,
                                "cyclaw-agent",
                                "模型调用完成",
                                serde_json::json!({
                                    "provider": provider_name.clone(),
                                    "role": "reviewer",
                                    "model_response_chars": result.response.chars().count(),
                                    "input_tokens": result.input_tokens,
                                    "output_tokens": result.output_tokens,
                                    "total_tokens": result.total_tokens,
                                    "latency_millis": result.latency_millis,
                                    "cache_hit": result.cache_hit,
                                    "trace_id": trace_id,
                                    "span_id": result.span_id,
                                }),
                            ),
                        )?;
                        response_summary = Some(summarize_model_response(&result.response));
                        match parse_model_reviews(&result.response) {
                            Ok(response) => {
                                let unknown = response.reviews.iter().find(|review| {
                                    !pending_candidates
                                        .iter()
                                        .any(|candidate| candidate.id == review.candidate_id)
                                });
                                if let Some(review) = unknown {
                                    review_error =
                                        Some(format!("模型返回未知候选: {}", review.candidate_id));
                                } else {
                                    let mut reviews = response.reviews;
                                    match load_or_default(&options.project_root) {
                                        Ok(policy) if policy.model_policy.review_critic_enabled => {
                                            let keep_count = reviews
                                                .iter()
                                                .filter(|review| review.recommendation == "keep")
                                                .count();
                                            if keep_count > 0 {
                                                match run_critic_review(
                                                    &options.project_root,
                                                    &provider_name,
                                                    &trace_id,
                                                    &model_span_id,
                                                    &reviews,
                                                ) {
                                                    Ok((verdicts, critic_result)) => {
                                                        append_event(
                                                            &options.project_root,
                                                            &new_event(
                                                                AgentEventType::ModelCalled,
                                                                "cyclaw-agent",
                                                                "怀疑者复核完成",
                                                                serde_json::json!({
                                                                    "provider": provider_name,
                                                                    "role": "critic",
                                                                    "input_tokens": critic_result.input_tokens,
                                                                    "output_tokens": critic_result.output_tokens,
                                                                    "total_tokens": critic_result.total_tokens,
                                                                    "latency_millis": critic_result.latency_millis,
                                                                    "cache_hit": critic_result.cache_hit,
                                                                    "trace_id": trace_id,
                                                                    "span_id": critic_result.span_id,
                                                                }),
                                                            ),
                                                        )?;
                                                        let (
                                                            merged,
                                                            confirmed,
                                                            downgraded,
                                                            rejected,
                                                        ) = apply_critic_verdicts(
                                                            reviews, verdicts,
                                                        );
                                                        reviews = merged;
                                                        critic_stats =
                                                            Some((confirmed, downgraded, rejected));
                                                    }
                                                    Err(error) => {
                                                        // 怀疑者失败不否决审查者结论，降级为单角色结果并留痕。
                                                        critic_error = Some(error.to_string());
                                                    }
                                                }
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            critic_error = Some(format!("策略读取失败: {error}"));
                                        }
                                    }
                                    for review in &reviews {
                                        if update_candidate_review(
                                            options.project_root.clone(),
                                            &review.candidate_id,
                                            review.confidence,
                                            review.recommendation.clone(),
                                            review.rationale.clone(),
                                        )
                                        .is_ok()
                                        {
                                            reviewed += 1;
                                        }
                                    }
                                }
                            }
                            Err(error) => review_error = Some(error.to_string()),
                        }
                    }
                    Err(error) => review_error = Some(error.to_string()),
                }
                model_provider = Some(provider_name.clone());
                model_response_summary = response_summary;
                if review_error.is_none() {
                    remove_run_state(&state_path)?;
                }
                let _ = append_trace_span(
                    &options.project_root,
                    &new_trace_span(
                        trace_id.clone(),
                        model_span_id.clone(),
                        Some(root_span_id.clone()),
                        "model_review",
                        TraceSpanKind::AgentRun,
                        "cyclaw-agent",
                        if review_error.is_none() {
                            TraceSpanStatus::Ok
                        } else {
                            TraceSpanStatus::Error
                        },
                        model_span_started_at,
                        Utc::now().to_rfc3339(),
                        serde_json::json!({
                            "provider": provider_name,
                            "reviewed": reviewed,
                            "error": review_error.clone(),
                            "critic_confirmed": critic_stats.as_ref().map(|stats| stats.0),
                            "critic_downgraded": critic_stats.as_ref().map(|stats| stats.1),
                            "critic_rejected": critic_stats.as_ref().map(|stats| stats.2),
                            "critic_failed": critic_error.clone(),
                        }),
                    ),
                );
                let candidate_ids = pending_candidates
                    .iter()
                    .map(|candidate| candidate.id.clone())
                    .collect::<Vec<_>>();
                if let Err(queue_error) = update_retry_task_after_review(
                    &options.project_root,
                    options.retry_task_id.as_deref(),
                    &trace_id,
                    &provider_name,
                    &candidate_ids,
                    review_error.is_none(),
                    review_error.as_deref(),
                ) {
                    steps.push(AgentStep {
                        name: "retry_task_update".to_string(),
                        status: AgentStepStatus::Failed,
                        detail: queue_error.to_string(),
                    });
                }
                steps.push(AgentStep {
                    name: "model_review".to_string(),
                    status: if review_error.is_none() {
                        AgentStepStatus::Completed
                    } else {
                        AgentStepStatus::Failed
                    },
                    detail: review_error.unwrap_or_else(|| {
                        let base = format!("provider={}, reviewed={}", provider_name, reviewed);
                        let stats = critic_stats
                            .map(|(confirmed, downgraded, rejected)| {
                                format!(
                                    ", confirmed={}, downgraded={}, rejected={}",
                                    confirmed, downgraded, rejected
                                )
                            })
                            .unwrap_or_default();
                        let critic = critic_error
                            .map(|error| format!(", critic_failed={}", error))
                            .unwrap_or_default();
                        format!("{}{}{}", base, stats, critic)
                    }),
                });
            }
            Ok(None) => {
                remove_run_state(&state_path)?;
                steps.push(AgentStep {
                    name: "model_review".to_string(),
                    status: AgentStepStatus::Skipped,
                    detail: "未配置 active 模型 Provider".to_string(),
                });
            }
            Err(error) => {
                steps.push(AgentStep {
                    name: "model_review".to_string(),
                    status: AgentStepStatus::Failed,
                    detail: error.to_string(),
                });
            }
        }
    } else {
        remove_run_state(&state_path)?;
        steps.push(AgentStep {
            name: "model_review".to_string(),
            status: AgentStepStatus::Skipped,
            detail: "本次运行禁用模型调用".to_string(),
        });
    }

    steps.push(AgentStep {
        name: "apply_model_reviewed_documents".to_string(),
        status: AgentStepStatus::Skipped,
        detail: "兼容批处理不会自动写入文档；请通过受控 Patch 审批".to_string(),
    });

    let completed_at = Utc::now();
    let record = AgentRunRecord {
        schema_version: 1,
        run_id: run_id.clone(),
        project_root: options.project_root.display().to_string(),
        started_at: started_at.to_rfc3339(),
        completed_at: completed_at.to_rfc3339(),
        changed,
        steps,
        pending_knowledge_count: pending_candidates.len(),
        pending_patch_count: pending_patches.len(),
        model_provider,
        model_response_summary,
        resumed,
        trace_id: Some(trace_id.clone()),
    };
    let record_path = write_record(&options.project_root, &record)?;
    append_event(
        &options.project_root,
        &new_event(
            AgentEventType::AgentRunCompleted,
            "cyclaw-agent",
            "Agent 运行完成",
            serde_json::json!({
                "run_id": record.run_id,
                "record_path": relative_path(&options.project_root, &record_path),
                "changed": record.changed,
                "pending_knowledge_count": record.pending_knowledge_count,
                "pending_patch_count": record.pending_patch_count,
                "model_provider": record.model_provider,
                "trace_id": record.trace_id,
            }),
        ),
    )?;
    let _ = append_trace_span(
        &options.project_root,
        &new_trace_span(
            trace_id,
            root_span_id,
            None,
            "agent_run",
            TraceSpanKind::AgentRun,
            "cyclaw-agent",
            TraceSpanStatus::Ok,
            run_started_at,
            completed_at.to_rfc3339(),
            serde_json::json!({
                "run_id": record.run_id,
                "resumed": record.resumed,
                "changed": record.changed,
                "pending_knowledge_count": record.pending_knowledge_count,
                "pending_patch_count": record.pending_patch_count,
                "model_provider": record.model_provider,
                "record_path": relative_path(&options.project_root, &record_path),
            }),
        ),
    );

    Ok(AgentRunResult {
        record_path,
        record,
    })
}

fn resolve_provider(project_root: &Path, provider: Option<String>) -> Result<Option<String>> {
    if provider.is_some() {
        return Ok(provider);
    }

    Ok(list_providers(project_root.to_path_buf())?.active_provider)
}

/// 模型审查结束后的重试队列状态回写：
/// - 有 retry_task_id（本次执行来自重试调度）：成功置 Completed，失败累加次数并按退避重排或耗尽降级；
/// - 无 retry_task_id 且失败（首次失败）：创建队列条目，等待退避后异步重试。
#[allow(clippy::too_many_arguments)]
fn update_retry_task_after_review(
    project_root: &Path,
    retry_task_id: Option<&str>,
    trace_id: &str,
    provider: &str,
    candidate_ids: &[String],
    success: bool,
    error: Option<&str>,
) -> Result<()> {
    let _lock = acquire_lock(
        project_root,
        "retry-queue",
        std::time::Duration::from_secs(5),
    )?;
    let policy = load_or_default(project_root)?;
    let retry_policy = &policy.model_policy;
    let mut queue = read_retry_queue(project_root)?;
    let now = Utc::now().to_rfc3339();
    let existing = retry_task_id
        .and_then(|id| queue.iter_mut().find(|task| task.id == id))
        .filter(|task| !matches!(task.status, RetryStatus::Completed | RetryStatus::Abandoned));

    if let Some(task) = existing {
        task.updated_at = now;
        if success {
            task.status = RetryStatus::Completed;
            task.completed_at = Some(task.updated_at.clone());
            task.last_error = None;
            let retry_id = task.id.clone();
            let trace = task.trace_id.clone();
            write_retry_queue(project_root, &queue)?;
            append_event(
                project_root,
                &new_event(
                    AgentEventType::RetryTaskCompleted,
                    "cyclaw-agent",
                    "重试任务完成",
                    serde_json::json!({
                        "retry_id": retry_id,
                        "trace_id": trace,
                        "kind": "model_review",
                    }),
                ),
            )?;
            return Ok(());
        }

        task.attempt_count = task.attempt_count.saturating_add(1);
        task.last_error = error.map(ToString::to_string);
        let retry_id = task.id.clone();
        let trace = task.trace_id.clone();
        let attempt = task.attempt_count;
        // 耗尽判定使用当前项目策略，配置变更立即对存量条目生效。
        let max_attempts = retry_policy.retry_max_attempts;
        task.max_attempts = max_attempts;
        if attempt >= max_attempts {
            task.status = RetryStatus::Exhausted;
            task.degraded = true;
            task.degraded_reason =
                Some("重试耗尽，模型审查降级为人工处理；候选保持待审状态".to_string());
            task.next_retry_at = None;
            write_retry_queue(project_root, &queue)?;
            append_event(
                project_root,
                &new_event(
                    AgentEventType::RetryTaskExhausted,
                    "cyclaw-agent",
                    "重试耗尽，任务已降级",
                    serde_json::json!({
                        "retry_id": retry_id,
                        "trace_id": trace,
                        "attempt_count": attempt,
                        "last_error": error,
                    }),
                ),
            )?;
        } else {
            task.status = RetryStatus::Pending;
            let delay = retry_backoff_seconds(
                attempt,
                retry_policy.retry_base_delay_seconds,
                retry_policy.retry_max_backoff_seconds,
            );
            task.next_retry_at = Some(next_retry_timestamp(delay));
            write_retry_queue(project_root, &queue)?;
        }
        return Ok(());
    }

    // 无对应条目：仅首次失败时入队；条目缺失的重试执行不重复入队。
    if success || retry_task_id.is_some() {
        return Ok(());
    }
    let mut task = new_retry_task(
        RetryKind::ModelReview,
        serde_json::json!({
            "provider": provider,
            "pending_candidate_ids": candidate_ids,
        }),
        Some(trace_id.to_string()),
    );
    task.attempt_count = 1;
    task.max_attempts = retry_policy.retry_max_attempts;
    task.last_error = error.map(ToString::to_string);
    let delay = retry_backoff_seconds(
        1,
        retry_policy.retry_base_delay_seconds,
        retry_policy.retry_max_backoff_seconds,
    );
    task.next_retry_at = Some(next_retry_timestamp(delay));
    let retry_id = task.id.clone();
    queue.push(task);
    write_retry_queue(project_root, &queue)?;
    append_event(
        project_root,
        &new_event(
            AgentEventType::RetryTaskScheduled,
            "cyclaw-agent",
            "失败任务已入重试队列",
            serde_json::json!({
                "retry_id": retry_id,
                "trace_id": trace_id,
                "kind": "model_review",
                "provider": provider,
                "attempt_count": 1,
            }),
        ),
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RetryRunOptions {
    pub project_root: PathBuf,
    /// 单次执行的重试任务上限。
    pub limit: usize,
}

impl RetryRunOptions {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryTaskOutcome {
    pub retry_id: String,
    pub status: RetryStatus,
    pub run_id: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct RetryRunResult {
    pub executed: usize,
    pub completed: usize,
    pub scheduled: usize,
    pub exhausted: usize,
    pub outcomes: Vec<RetryTaskOutcome>,
}

/// 执行所有到期的重试任务：恢复失败上下文后重新运行 Agent 审查。
pub fn run_due_retries(options: RetryRunOptions) -> Result<RetryRunResult> {
    ensure_directory(&options.project_root)?;
    let _run_lock = acquire_lock(
        &options.project_root,
        "retry-run",
        std::time::Duration::from_secs(5),
    )?;
    let now = Utc::now();
    let due = {
        let _queue_lock = acquire_lock(
            &options.project_root,
            "retry-queue",
            std::time::Duration::from_secs(5),
        )?;
        read_retry_queue(&options.project_root)?
            .into_iter()
            .filter(|task| task.status == RetryStatus::Pending)
            .filter(|task| {
                task.next_retry_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|time| time.with_timezone(&Utc) <= now)
                    .unwrap_or(true)
            })
            .take(options.limit.max(1))
            .map(|task| (task.id.clone(), task.payload.clone()))
            .collect::<Vec<_>>()
    };

    let mut result = RetryRunResult {
        executed: 0,
        completed: 0,
        scheduled: 0,
        exhausted: 0,
        outcomes: Vec::new(),
    };
    for (retry_id, payload) in due {
        let provider = payload
            .get("provider")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let run_options = AgentRunOptions {
            project_root: options.project_root.clone(),
            provider,
            use_model: true,
            retry_task_id: Some(retry_id.clone()),
        };
        result.executed += 1;
        let run_result = run_agent_once(run_options);
        let task = read_retry_queue(&options.project_root)?
            .into_iter()
            .find(|task| task.id == retry_id);
        match run_result {
            Ok(run) => {
                let status = task.map(|task| task.status).unwrap_or(RetryStatus::Pending);
                match status {
                    RetryStatus::Completed => {
                        result.completed += 1;
                        result.outcomes.push(RetryTaskOutcome {
                            retry_id,
                            status,
                            run_id: Some(run.record.run_id),
                            detail: "重试成功，模型审查完成".to_string(),
                        });
                    }
                    RetryStatus::Exhausted => {
                        result.exhausted += 1;
                        result.outcomes.push(RetryTaskOutcome {
                            retry_id,
                            status,
                            run_id: Some(run.record.run_id),
                            detail: "重试耗尽，已降级为人工处理".to_string(),
                        });
                    }
                    _ => {
                        result.scheduled += 1;
                        result.outcomes.push(RetryTaskOutcome {
                            retry_id,
                            status,
                            run_id: Some(run.record.run_id),
                            detail: "重试失败，已按退避计划安排下次重试".to_string(),
                        });
                    }
                }
            }
            Err(error) => {
                let status =
                    mark_retry_failure(&options.project_root, &retry_id, &error.to_string())?;
                if status == RetryStatus::Exhausted {
                    result.exhausted += 1;
                    result.outcomes.push(RetryTaskOutcome {
                        retry_id,
                        status,
                        run_id: None,
                        detail: format!("重试执行异常且耗尽，已降级: {error}"),
                    });
                } else {
                    result.scheduled += 1;
                    result.outcomes.push(RetryTaskOutcome {
                        retry_id,
                        status,
                        run_id: None,
                        detail: format!("重试执行异常，已按退避计划安排下次重试: {error}"),
                    });
                }
            }
        }
    }
    Ok(result)
}

/// 重试执行在 Agent run 提前失败（如锁竞争、项目状态错误）时的兜底回写。
fn mark_retry_failure(project_root: &Path, retry_id: &str, error: &str) -> Result<RetryStatus> {
    let _lock = acquire_lock(
        project_root,
        "retry-queue",
        std::time::Duration::from_secs(5),
    )?;
    let mut queue = read_retry_queue(project_root)?;
    let Some(task) = queue.iter_mut().find(|task| task.id == retry_id) else {
        return Ok(RetryStatus::Pending);
    };
    task.updated_at = Utc::now().to_rfc3339();
    task.attempt_count = task.attempt_count.saturating_add(1);
    task.last_error = Some(error.to_string());
    let attempt = task.attempt_count;
    let trace = task.trace_id.clone();
    let policy = load_or_default(project_root)?;
    task.max_attempts = policy.model_policy.retry_max_attempts;
    if attempt >= task.max_attempts {
        task.status = RetryStatus::Exhausted;
        task.degraded = true;
        task.degraded_reason =
            Some("重试耗尽，模型审查降级为人工处理；候选保持待审状态".to_string());
        task.next_retry_at = None;
        write_retry_queue(project_root, &queue)?;
        append_event(
            project_root,
            &new_event(
                AgentEventType::RetryTaskExhausted,
                "cyclaw-agent",
                "重试耗尽，任务已降级",
                serde_json::json!({
                    "retry_id": retry_id,
                    "trace_id": trace,
                    "attempt_count": attempt,
                    "last_error": error,
                }),
            ),
        )?;
        return Ok(RetryStatus::Exhausted);
    }
    task.status = RetryStatus::Pending;
    let delay = retry_backoff_seconds(
        task.attempt_count,
        policy.model_policy.retry_base_delay_seconds,
        policy.model_policy.retry_max_backoff_seconds,
    );
    task.next_retry_at = Some(next_retry_timestamp(delay));
    write_retry_queue(project_root, &queue)?;
    Ok(RetryStatus::Pending)
}

/// 人工放弃一条重试任务（例如确认是逻辑错误而不是瞬态故障）。
pub fn abandon_retry_task(project_root: &Path, retry_id: &str) -> Result<RetryTaskOutcome> {
    ensure_directory(project_root)?;
    let _lock = acquire_lock(
        project_root,
        "retry-queue",
        std::time::Duration::from_secs(5),
    )?;
    let mut queue = read_retry_queue(project_root)?;
    let task = queue
        .iter_mut()
        .find(|task| task.id == retry_id)
        .with_context(|| format!("未找到重试任务: {retry_id}"))?;
    task.status = RetryStatus::Abandoned;
    task.updated_at = Utc::now().to_rfc3339();
    task.degraded = true;
    task.degraded_reason = Some("人工放弃自动重试，降级为人工处理".to_string());
    let status = task.status.clone();
    let detail = task.degraded_reason.clone().unwrap_or_default();
    let trace_id = task.trace_id.clone();
    write_retry_queue(project_root, &queue)?;
    append_event(
        project_root,
        &new_event(
            AgentEventType::RetryTaskExhausted,
            "cyclaw-agent",
            "重试任务已人工放弃",
            serde_json::json!({
                "retry_id": retry_id,
                "trace_id": trace_id,
                "kind": "manual_abandon",
            }),
        ),
    )?;
    Ok(RetryTaskOutcome {
        retry_id: retry_id.to_string(),
        status,
        run_id: None,
        detail,
    })
}

fn render_agent_prompt(
    candidates: &[cyclaw_knowledge::KnowledgeCandidate],
    patches: &[cyclaw_docs::DocumentPatch],
) -> String {
    let candidate_lines = candidates
        .iter()
        .map(|candidate| {
            format!(
                "- id={} | {} -> {} | 规则置信度={} | 依据={}",
                candidate.id,
                candidate.summary,
                candidate.recommended_doc,
                candidate.confidence,
                candidate.reasons.join("；")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let patch_lines = patches
        .iter()
        .map(|patch| format!("- {} -> {}", patch.summary, patch.target_doc))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"请作为 cyClaw 项目知识 Agent，审查以下候选知识。
只输出一个 JSON 对象，不要输出 Markdown 或额外说明。格式必须是：
{{"reviews":[{{"candidate_id":"候选ID","recommendation":"keep|ignore","confidence":0到100的整数,"rationale":"中文理由"}}]}}

confidence 表示该候选确实值得沉淀到目标文档的置信度。重复、噪声或无文档价值的候选应推荐 ignore 并给出较低 confidence。

候选知识：
{}

文档草稿：
{}
"#,
        if candidate_lines.is_empty() {
            "无".to_string()
        } else {
            candidate_lines
        },
        if patch_lines.is_empty() {
            "无".to_string()
        } else {
            patch_lines
        }
    )
}

fn parse_model_reviews(response: &str) -> Result<ModelReviewResponse> {
    serde_json::from_str(strip_json_fence(response)).context("模型未返回合法的候选审查 JSON")
}

fn strip_json_fence(response: &str) -> &str {
    let trimmed = response.trim();
    if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    }
}

fn parse_critic_verdicts(response: &str) -> Result<CriticResponse> {
    serde_json::from_str(strip_json_fence(response)).context("怀疑者未返回合法的复核 JSON")
}

/// 渲染怀疑者提示词：只复核审查者推荐 keep 的候选，降低复核成本。
fn render_critic_prompt(reviews: &[ModelCandidateReview]) -> String {
    let lines = reviews
        .iter()
        .filter(|review| review.recommendation == "keep")
        .map(|review| {
            format!(
                "- id={} | reviewer_confidence={} | reviewer_rationale={}",
                review.candidate_id, review.confidence, review.rationale
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"请作为 cyClaw 候选知识的对抗性复核者（怀疑者）。审查者推荐将以下候选沉淀到项目文档，请逐条质疑：
候选是否真的值得沉淀？理由是否站得住脚？是否存在重复、噪声或过度自信？

只输出一个 JSON 对象，不要输出 Markdown 或额外说明。格式：
{{"verdicts":[{{"candidate_id":"候选ID","verdict":"confirm|downgrade|reject","confidence":0到100的整数,"rationale":"中文理由"}}]}}

verdict 含义：confirm=同意沉淀；downgrade=可沉淀但置信度应下调；reject=不应沉淀。
对下面每一行候选都必须给出 verdict，不得遗漏。

审查者结论：
{}
"#,
        if lines.is_empty() {
            "无".to_string()
        } else {
            lines
        }
    )
}

/// 怀疑者结论与审查者结论的确定性融合：
/// confirm 不变；downgrade 取更低置信度；reject 改为 ignore 并取更低置信度。
fn apply_critic_verdicts(
    mut reviews: Vec<ModelCandidateReview>,
    verdicts: Vec<CriticVerdict>,
) -> (Vec<ModelCandidateReview>, usize, usize, usize) {
    let mut confirmed = 0;
    let mut downgraded = 0;
    let mut rejected = 0;
    for review in reviews.iter_mut() {
        if review.recommendation != "keep" {
            continue;
        }
        let Some(verdict) = verdicts
            .iter()
            .find(|verdict| verdict.candidate_id == review.candidate_id)
        else {
            continue;
        };
        match verdict.verdict.as_str() {
            "confirm" => confirmed += 1,
            "downgrade" => {
                review.confidence = review.confidence.min(verdict.confidence);
                review.rationale = format!(
                    "怀疑者下调: {}；审查者: {}",
                    truncate_rationale(&verdict.rationale),
                    review.rationale
                );
                downgraded += 1;
            }
            "reject" => {
                review.confidence = review.confidence.min(verdict.confidence);
                review.recommendation = "ignore".to_string();
                review.rationale = format!(
                    "怀疑者否决: {}；审查者: {}",
                    truncate_rationale(&verdict.rationale),
                    review.rationale
                );
                rejected += 1;
            }
            _ => {}
        }
    }
    (reviews, confirmed, downgraded, rejected)
}

fn truncate_rationale(text: &str) -> String {
    text.chars().take(120).collect()
}

/// 怀疑者复核：作为 model_review 的兄弟调用挂入同一 trace，phase 区分角色。
fn run_critic_review(
    project_root: &Path,
    provider_name: &str,
    trace_id: &str,
    parent_span_id: &str,
    reviews: &[ModelCandidateReview],
) -> Result<(Vec<CriticVerdict>, TestProviderResult)> {
    let result = test_provider(TestProviderOptions {
        project_root: project_root.to_path_buf(),
        name: provider_name.to_string(),
        prompt: render_critic_prompt(reviews),
        trace: Some(TraceContext {
            trace_id: trace_id.to_string(),
            parent_span_id: Some(parent_span_id.to_string()),
            phase: Some("agent_review_critic".to_string()),
        }),
    })?;
    let parsed = parse_critic_verdicts(&result.response)?;
    Ok((parsed.verdicts, result))
}

fn summarize_model_response(response: &str) -> String {
    let normalized = response.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = normalized.chars().take(240).collect::<String>();
    format!("chars={} preview={}", response.chars().count(), preview)
}

fn write_record(project_root: &Path, record: &AgentRunRecord) -> Result<PathBuf> {
    let runs_dir = project_root.join(CYCLE_DIR).join("agent-runs");
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("无法创建 Agent 运行目录: {}", runs_dir.display()))?;
    let record_path = runs_dir.join(format!("{}.json", record.run_id));
    let json = serde_json::to_string_pretty(record)?;
    fs::write(&record_path, json)
        .with_context(|| format!("无法写入 Agent 运行记录: {}", record_path.display()))?;
    Ok(record_path)
}

fn run_state_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("agent-run-state.json")
}

fn read_run_state(path: &Path) -> Result<Option<AgentRunState>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("无法读取 Agent 恢复状态: {}", path.display()))?;
    Ok(Some(serde_json::from_str(&content)?))
}

fn write_run_state(path: &Path, state: &AgentRunState) -> Result<()> {
    let parent = path.parent().context("Agent 恢复状态缺少父目录")?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file(), state)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn remove_run_state(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("无法清理 Agent 恢复状态: {}", path.display()))?;
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!("项目根目录不存在或不是目录: {}", path.display());
    }
    Ok(())
}

fn relative_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|relative| relative.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_run_without_model_writes_record() {
        let temp = tempfile::tempdir().unwrap();
        let result =
            run_agent_once(AgentRunOptions::new(temp.path().to_path_buf(), None, false)).unwrap();

        assert!(result.record_path.exists());
        assert_eq!(result.record.model_provider, None);
        assert!(
            result
                .record
                .steps
                .iter()
                .any(|step| step.name == "model_review")
        );
    }

    #[test]
    fn parses_fenced_model_review_json() {
        let response = r#"```json
{"reviews":[{"candidate_id":"kc-1","recommendation":"keep","confidence":92,"rationale":"接口发生变化"}]}
```"#;
        let parsed = parse_model_reviews(response).unwrap();

        assert_eq!(parsed.reviews.len(), 1);
        assert_eq!(parsed.reviews[0].confidence, 92);
        assert_eq!(parsed.reviews[0].recommendation, "keep");
    }

    #[test]
    fn first_review_failure_enqueues_retry_task() {
        let temp = tempfile::tempdir().unwrap();
        update_retry_task_after_review(
            temp.path(),
            None,
            "agent-1",
            "deepseek",
            &["kc-1".to_string()],
            false,
            Some("模型请求失败"),
        )
        .unwrap();

        let queue = read_retry_queue(temp.path()).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].status, RetryStatus::Pending);
        assert_eq!(queue[0].attempt_count, 1);
        assert_eq!(queue[0].kind, RetryKind::ModelReview);
        assert!(queue[0].next_retry_at.is_some());
        assert_eq!(queue[0].trace_id.as_deref(), Some("agent-1"));
        // 成功执行不产生条目
        update_retry_task_after_review(temp.path(), None, "agent-2", "deepseek", &[], true, None)
            .unwrap();
        assert_eq!(read_retry_queue(temp.path()).unwrap().len(), 1);
    }

    #[test]
    fn retry_task_exhausts_after_max_attempts_and_degrades() {
        let temp = tempfile::tempdir().unwrap();
        update_retry_task_after_review(
            temp.path(),
            None,
            "agent-1",
            "deepseek",
            &[],
            false,
            Some("首次失败"),
        )
        .unwrap();
        let retry_id = read_retry_queue(temp.path()).unwrap()[0].id.clone();

        let max_attempts = read_retry_queue(temp.path()).unwrap()[0].max_attempts;
        for attempt in 1..max_attempts {
            update_retry_task_after_review(
                temp.path(),
                Some(&retry_id),
                "agent-1",
                "deepseek",
                &[],
                false,
                Some("再次失败"),
            )
            .unwrap();
            let task = &read_retry_queue(temp.path()).unwrap()[0];
            assert_eq!(task.attempt_count, attempt + 1);
            if attempt + 1 < max_attempts {
                assert_eq!(task.status, RetryStatus::Pending);
            }
        }

        let task = &read_retry_queue(temp.path()).unwrap()[0];
        assert_eq!(task.status, RetryStatus::Exhausted);
        assert!(task.degraded);
        assert!(task.degraded_reason.is_some());
    }

    #[test]
    fn retry_run_completes_task_when_review_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        update_retry_task_after_review(
            temp.path(),
            None,
            "agent-1",
            "deepseek",
            &[],
            false,
            Some("首次失败"),
        )
        .unwrap();
        let retry_id = read_retry_queue(temp.path()).unwrap()[0].id.clone();
        // 模拟重试成功：update 以 retry_task_id 命中条目并标记完成
        update_retry_task_after_review(
            temp.path(),
            Some(&retry_id),
            "agent-1",
            "deepseek",
            &[],
            true,
            None,
        )
        .unwrap();

        let task = &read_retry_queue(temp.path()).unwrap()[0];
        assert_eq!(task.status, RetryStatus::Completed);
        assert!(task.completed_at.is_some());
        assert!(!task.degraded);
    }

    fn sample_review(id: &str, recommendation: &str, confidence: u8) -> ModelCandidateReview {
        ModelCandidateReview {
            candidate_id: id.to_string(),
            recommendation: recommendation.to_string(),
            confidence,
            rationale: format!("审查者对 {} 的理由", id),
        }
    }

    fn sample_verdict(id: &str, verdict: &str, confidence: u8) -> CriticVerdict {
        CriticVerdict {
            candidate_id: id.to_string(),
            verdict: verdict.to_string(),
            confidence,
            rationale: format!("怀疑者对 {} 的质疑", id),
        }
    }

    #[test]
    fn critic_verdicts_merge_deterministically() {
        let reviews = vec![
            sample_review("kc-1", "keep", 90),
            sample_review("kc-2", "keep", 70),
            sample_review("kc-3", "keep", 80),
            sample_review("kc-4", "ignore", 30),
        ];
        let verdicts = vec![
            sample_verdict("kc-1", "confirm", 95),
            sample_verdict("kc-2", "downgrade", 40),
            sample_verdict("kc-3", "reject", 20),
            sample_verdict("kc-4", "reject", 10), // 非 keep 候选不复核
            sample_verdict("kc-99", "confirm", 99), // 未知 id 忽略
            sample_verdict("kc-1", "unknown-verdict", 1), // 非法 verdict 忽略
        ];

        let (merged, confirmed, downgraded, rejected) = apply_critic_verdicts(reviews, verdicts);

        assert_eq!(confirmed, 1);
        assert_eq!(downgraded, 1);
        assert_eq!(rejected, 1);
        assert_eq!(merged[0].recommendation, "keep");
        assert_eq!(merged[0].confidence, 90);
        assert_eq!(merged[1].confidence, 40);
        assert!(merged[1].rationale.starts_with("怀疑者下调"));
        assert_eq!(merged[2].recommendation, "ignore");
        assert_eq!(merged[2].confidence, 20);
        assert!(merged[2].rationale.starts_with("怀疑者否决"));
        // ignore 候选与重复/非法 verdict 不影响结果
        assert_eq!(merged[3].recommendation, "ignore");
        assert_eq!(merged[3].confidence, 30);
    }

    #[test]
    fn critic_prompt_only_contains_keep_candidates() {
        let reviews = vec![
            sample_review("kc-1", "keep", 90),
            sample_review("kc-2", "ignore", 30),
        ];
        let prompt = render_critic_prompt(&reviews);

        assert!(prompt.contains("kc-1"));
        assert!(!prompt.contains("kc-2"));
        assert!(prompt.contains("对抗性复核"));
        assert!(prompt.contains("confirm|downgrade|reject"));
    }

    #[test]
    fn parses_fenced_critic_json() {
        let response = r#"```json
{"verdicts":[{"candidate_id":"kc-1","verdict":"downgrade","confidence":35,"rationale":"重复候选"}]}
```"#;
        let parsed = parse_critic_verdicts(response).unwrap();

        assert_eq!(parsed.verdicts.len(), 1);
        assert_eq!(parsed.verdicts[0].verdict, "downgrade");
        assert_eq!(parsed.verdicts[0].confidence, 35);
    }
}
