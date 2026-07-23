use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cyclaw_events::{AgentEventType, append_event, new_event};
use cyclaw_policy::acquire_lock;
use serde::{Deserialize, Serialize};

const CYCLE_DIR: &str = ".cyclaw";

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    pub title: String,
    pub objective: String,
    pub status: TaskStatus,
    pub related_files: Vec<String>,
    pub context_budget_tokens: usize,
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
    pub git_head: Option<String>,
}

impl BeginTaskOptions {
    pub fn new(project_root: PathBuf, title: String, objective: String) -> Self {
        Self {
            project_root,
            title,
            objective,
            related_files: Vec::new(),
            context_budget_tokens: 2_000,
            git_head: None,
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
    pub source_task_id: Option<String>,
    pub confidence: u8,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub supersedes: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_verified_at: String,
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
    pub created_at: String,
}

pub fn begin_task(options: BeginTaskOptions) -> Result<TaskRecord> {
    ensure_memory_dirs(&options.project_root)?;
    let _lock = acquire_lock(&options.project_root, "memory", Duration::from_secs(5))?;
    if let Some(active) = read_active_task_id(&options.project_root)? {
        anyhow::bail!("已有活动任务，请先关闭: {}", active);
    }
    let now = Utc::now().to_rfc3339();
    let task = TaskRecord {
        id: task_id(&options.title, &now),
        title: options.title,
        objective: options.objective,
        status: TaskStatus::Active,
        related_files: dedupe_strings(options.related_files),
        context_budget_tokens: options.context_budget_tokens.clamp(256, 16_000),
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
    fs::write(active_task_path(&options.project_root), &task.id)?;
    record_event(
        &options.project_root,
        AgentEventType::TaskStarted,
        "开始项目任务",
        serde_json::json!({"task_id":task.id,"title":task.title}),
    )?;
    Ok(task)
}

pub fn get_active_task(project_root: &Path) -> Result<Option<TaskRecord>> {
    let Some(id) = read_active_task_id(project_root)? else {
        return Ok(None);
    };
    Ok(Some(read_task(project_root, &id)?))
}

pub fn get_task(project_root: &Path, task_id: &str) -> Result<TaskRecord> {
    read_task(project_root, task_id)
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
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id(project_root, task_id)?;
    let mut task = read_task(project_root, &id)?;
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
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id(project_root, task_id)?;
    let mut task = read_task(project_root, &id)?;
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
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id(project_root, task_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    let files = dedupe_strings(related_files);
    task.related_files.extend(files.clone());
    task.related_files = dedupe_strings(task.related_files);
    task.checkpoints.push(TaskCheckpoint {
        id: item_id("checkpoint", &summary, &now),
        summary,
        related_files: files,
        created_at: now.clone(),
    });
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
    ensure_memory_dirs(project_root)?;
    let _lock = acquire_lock(project_root, "memory", Duration::from_secs(5))?;
    let id = resolve_task_id(project_root, task_id)?;
    let mut task = read_task(project_root, &id)?;
    ensure_active(&task)?;
    let now = Utc::now().to_rfc3339();
    task.status = TaskStatus::Closed;
    task.summary = Some(summary);
    task.updated_at = now.clone();
    task.closed_at = Some(now);
    write_task(project_root, &task)?;
    if read_active_task_id(project_root)?.as_deref() == Some(id.as_str()) {
        let path = active_task_path(project_root);
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
            let fact_tokens = tokens(&fact.statement);
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
        if !fact.evidence.is_empty()
            && fact
                .evidence
                .iter()
                .filter(|value| looks_like_path(value))
                .all(|value| !evidence_exists(project_root, value))
        {
            findings.push(finding(
                ReconciliationKind::Stale,
                vec![fact.id.clone()],
                "事实关联的证据路径均已不存在".to_string(),
                "delete",
                90,
            ));
        } else if fact
            .valid_until
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value < Utc::now())
        {
            findings.push(finding(
                ReconciliationKind::Stale,
                vec![fact.id.clone()],
                "事实已经超过有效期限".to_string(),
                "delete",
                95,
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
            "stale":report.stale_count
        }),
    )?;
    Ok(report)
}

pub fn latest_reconciliation(project_root: &Path) -> Result<Option<ReconciliationReport>> {
    let dir = reconciliation_dir(project_root);
    if !dir.exists() {
        return Ok(None);
    }
    let mut paths = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    let Some(path) = paths.pop() else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
}

fn upsert_fact(project_root: &Path, fact: ProjectFact) -> Result<ProjectFact> {
    let mut facts = list_facts(project_root)?;
    if let Some(existing) = facts
        .iter_mut()
        .find(|value| value.statement == fact.statement && value.status == FactStatus::Active)
    {
        existing.evidence.extend(fact.evidence);
        existing.evidence = dedupe_strings(std::mem::take(&mut existing.evidence));
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
    fs::write(facts_path(project_root), format!("{}\n", content))?;
    Ok(())
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

fn resolve_task_id(project_root: &Path, task_id: Option<&str>) -> Result<String> {
    if let Some(id) = task_id {
        return Ok(id.to_string());
    }
    read_active_task_id(project_root)?.context("当前没有活动任务，请先调用 begin_task")
}

fn ensure_active(task: &TaskRecord) -> Result<()> {
    if task.status != TaskStatus::Active {
        anyhow::bail!("任务已经关闭: {}", task.id);
    }
    Ok(())
}

fn read_active_task_id(project_root: &Path) -> Result<Option<String>> {
    let path = active_task_path(project_root);
    if !path.exists() {
        return Ok(None);
    }
    let id = fs::read_to_string(path)?.trim().to_string();
    Ok((!id.is_empty()).then_some(id))
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

fn memory_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("memory")
}

fn facts_path(project_root: &Path) -> PathBuf {
    memory_dir(project_root).join("facts.jsonl")
}

fn reconciliation_dir(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("reconciliation")
}

fn task_id(title: &str, now: &str) -> String {
    let slug = title
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || *value == '-')
        .take(32)
        .collect::<String>();
    format!(
        "task_{}_{}",
        Utc::now().format("%Y%m%d_%H%M%S"),
        if slug.is_empty() { "work" } else { &slug }
    ) + &format!("_{:06x}", digest(&(title, now)) as u32 & 0x00ff_ffff)
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

fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.contains('.')
}

fn evidence_exists(project_root: &Path, value: &str) -> bool {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path.exists()
    } else {
        project_root.join(path).exists()
    }
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
}
