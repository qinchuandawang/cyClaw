use anyhow::Result;
use chrono::Utc;
use cyclaw_knowledge::{KnowledgeCandidate, KnowledgeImportance};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentPatch {
    pub id: String,
    pub candidate_id: String,
    pub target_doc: String,
    pub status: DocumentPatchStatus,
    pub summary: String,
    pub original_content: String,
    pub proposed_content: String,
    pub preview: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentPatchStatus {
    Pending,
    Applied,
}

pub fn create_document_patch(
    candidate: &KnowledgeCandidate,
    original_content: &str,
) -> DocumentPatch {
    let now = Utc::now().to_rfc3339();
    let section = render_candidate_section(candidate);
    let proposed_content = append_section(original_content, &section);

    DocumentPatch {
        id: patch_id(candidate),
        candidate_id: candidate.id.clone(),
        target_doc: candidate.recommended_doc.clone(),
        status: DocumentPatchStatus::Pending,
        summary: format!("更新 {}", candidate.recommended_doc),
        original_content: original_content.to_string(),
        proposed_content: proposed_content.clone(),
        preview: render_preview(candidate, &section),
        created_at: now.clone(),
        updated_at: now,
    }
}

pub fn parse_patch(content: &str) -> Result<DocumentPatch> {
    Ok(serde_json::from_str(content)?)
}

pub fn render_patch(patch: &DocumentPatch) -> Result<String> {
    Ok(serde_json::to_string_pretty(patch)?)
}

pub fn mark_applied(patch: &mut DocumentPatch) {
    patch.status = DocumentPatchStatus::Applied;
    patch.updated_at = Utc::now().to_rfc3339();
}

fn patch_id(candidate: &KnowledgeCandidate) -> String {
    format!("patch_{}", candidate.id)
}

fn append_section(original_content: &str, section: &str) -> String {
    if original_content.contains(section) {
        return original_content.to_string();
    }

    let mut content = original_content.trim_end().to_string();
    if !content.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(section);
    content.push('\n');
    content
}

fn render_candidate_section(candidate: &KnowledgeCandidate) -> String {
    let reasons = candidate
        .reasons
        .iter()
        .map(|reason| format!("- {}", reason))
        .collect::<Vec<_>>()
        .join("\n");
    let related_files = candidate
        .related_files
        .iter()
        .map(|file| format!("- `{}`", file))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"## {}

来源候选：`{}`

重要性：{}

{}

### 关联文件

{}
"#,
        candidate.summary,
        candidate.id,
        render_importance(&candidate.importance),
        reasons,
        if related_files.is_empty() {
            "- 暂无".to_string()
        } else {
            related_files
        }
    )
}

fn render_preview(candidate: &KnowledgeCandidate, section: &str) -> String {
    format!(
        "目标文档: {}\n候选知识: {}\n\n将追加以下内容:\n\n{}",
        candidate.recommended_doc, candidate.id, section
    )
}

fn render_importance(importance: &KnowledgeImportance) -> &'static str {
    match importance {
        KnowledgeImportance::High => "高",
        KnowledgeImportance::Medium => "中",
        KnowledgeImportance::Low => "低",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cyclaw_knowledge::{
        KnowledgeCandidate, KnowledgeImportance, KnowledgeSourceType, KnowledgeStatus,
    };

    #[test]
    fn creates_append_only_patch() {
        let candidate = KnowledgeCandidate {
            id: "kc_001".to_string(),
            summary: "建议更新 docs/dependencies.md：依赖变化".to_string(),
            source_type: KnowledgeSourceType::ChangeAnalysis,
            source_ref: ".cyclaw/runs/demo/change-analysis.json".to_string(),
            importance: KnowledgeImportance::Medium,
            reasons: vec!["依赖变化可能影响安装、构建或运行环境".to_string()],
            recommended_doc: "docs/dependencies.md".to_string(),
            related_files: vec!["Cargo.toml".to_string()],
            status: KnowledgeStatus::Pending,
            created_at: "2026-07-06T00:00:00Z".to_string(),
            updated_at: "2026-07-06T00:00:00Z".to_string(),
        };

        let patch = create_document_patch(&candidate, "# Dependencies\n");

        assert_eq!(patch.target_doc, "docs/dependencies.md");
        assert!(patch.proposed_content.contains("来源候选：`kc_001`"));
        assert!(patch.proposed_content.contains("`Cargo.toml`"));
    }
}
