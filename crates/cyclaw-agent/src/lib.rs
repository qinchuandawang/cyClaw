use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_core::{
    list_document_patches, list_inbox, project_status, update_candidate_review, watch_project_once,
};
use cyclaw_docs::DocumentPatchStatus;
use cyclaw_events::{AgentEventType, append_event, new_event, new_id};
use cyclaw_knowledge::KnowledgeStatus;
use cyclaw_model::{TestProviderOptions, list_providers, test_provider};
use serde::{Deserialize, Serialize};

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

const CYCLE_DIR: &str = ".cyclaw";

#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub project_root: PathBuf,
    pub provider: Option<String>,
    pub use_model: bool,
}

impl AgentRunOptions {
    pub fn new(project_root: PathBuf, provider: Option<String>, use_model: bool) -> Self {
        Self {
            project_root,
            provider,
            use_model,
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
    let started_at = Utc::now();
    let run_id = new_id("agent");
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
                let prompt = render_agent_prompt(&pending_candidates, &pending_patches);
                let result = test_provider(TestProviderOptions {
                    project_root: options.project_root.clone(),
                    name: provider_name.clone(),
                    prompt,
                })?;
                append_event(
                    &options.project_root,
                    &new_event(
                        AgentEventType::ModelCalled,
                        "cyclaw-agent",
                        "模型调用完成",
                        serde_json::json!({
                            "provider": provider_name.clone(),
                            "model_response_chars": result.response.chars().count(),
                            "input_tokens": result.input_tokens,
                            "output_tokens": result.output_tokens,
                            "total_tokens": result.total_tokens,
                            "latency_millis": result.latency_millis,
                            "cache_hit": result.cache_hit,
                        }),
                    ),
                )?;
                let review_result = parse_model_reviews(&result.response);
                let mut reviewed = 0;
                let mut review_error = None;
                match review_result {
                    Ok(response) => {
                        for review in response.reviews {
                            if !pending_candidates
                                .iter()
                                .any(|candidate| candidate.id == review.candidate_id)
                            {
                                review_error =
                                    Some(format!("模型返回未知候选: {}", review.candidate_id));
                                break;
                            }
                            if update_candidate_review(
                                options.project_root.clone(),
                                &review.candidate_id,
                                review.confidence,
                                review.recommendation,
                                review.rationale,
                            )
                            .is_ok()
                            {
                                reviewed += 1;
                            }
                        }
                    }
                    Err(error) => review_error = Some(error.to_string()),
                }
                model_provider = Some(provider_name.clone());
                model_response_summary = Some(summarize_model_response(&result.response));
                steps.push(AgentStep {
                    name: "model_review".to_string(),
                    status: if review_error.is_none() {
                        AgentStepStatus::Completed
                    } else {
                        AgentStepStatus::Failed
                    },
                    detail: review_error.unwrap_or_else(|| {
                        format!("provider={}, reviewed={}", provider_name, reviewed)
                    }),
                });
            }
            Ok(None) => {
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
            }),
        ),
    )?;

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
    let trimmed = response.trim();
    let json = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    serde_json::from_str(json).context("模型未返回合法的候选审查 JSON")
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
}
