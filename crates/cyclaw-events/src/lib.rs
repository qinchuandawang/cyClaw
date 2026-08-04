use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

const CYCLE_DIR: &str = ".cyclaw";
const EVENTS_FILE: &str = "events.jsonl";

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
        events.push(serde_json::from_str(line)?);
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
        id: format!("event-{}", now.format("%Y%m%d%H%M%S%3f")),
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
}
