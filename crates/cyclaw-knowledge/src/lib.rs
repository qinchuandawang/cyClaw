use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;
use chrono::Utc;
use cyclaw_change_radar::{ChangeAnalysis, ChangeType, ImpactedAsset};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeCandidate {
    pub id: String,
    pub summary: String,
    pub source_type: KnowledgeSourceType,
    pub source_ref: String,
    pub importance: KnowledgeImportance,
    pub reasons: Vec<String>,
    pub recommended_doc: String,
    pub related_files: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: u8,
    #[serde(default)]
    pub reviewed_by_model: bool,
    #[serde(default)]
    pub model_recommendation: Option<String>,
    #[serde(default)]
    pub model_rationale: Option<String>,
    pub status: KnowledgeStatus,
    pub created_at: String,
    pub updated_at: String,
}

fn default_confidence() -> u8 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceType {
    ChangeAnalysis,
    TaskSummary,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeImportance {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeStatus {
    Pending,
    Accepted,
    Ignored,
}

pub fn candidates_from_change_analysis(
    analysis: &ChangeAnalysis,
    source_ref: &str,
) -> Vec<KnowledgeCandidate> {
    analysis
        .impacted_assets
        .iter()
        .enumerate()
        .map(|(index, asset)| candidate_from_asset(analysis, source_ref, asset, index))
        .collect()
}

pub fn parse_jsonl(content: &str) -> Result<Vec<KnowledgeCandidate>> {
    let mut candidates = Vec::new();

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        candidates.push(serde_json::from_str(line)?);
    }

    Ok(candidates)
}

pub fn render_jsonl(candidates: &[KnowledgeCandidate]) -> Result<String> {
    let mut lines = Vec::new();

    for candidate in candidates {
        lines.push(serde_json::to_string(candidate)?);
    }

    Ok(format!("{}\n", lines.join("\n")))
}

fn candidate_from_asset(
    analysis: &ChangeAnalysis,
    source_ref: &str,
    asset: &ImpactedAsset,
    index: usize,
) -> KnowledgeCandidate {
    let now = Utc::now().to_rfc3339();
    let related_change_types = related_change_types(analysis, &asset.related_files);
    let importance = score_importance(&related_change_types, asset);
    let confidence = confidence_for_importance(&importance);

    KnowledgeCandidate {
        id: candidate_id(analysis, asset, index),
        summary: summary_for_asset(asset),
        source_type: KnowledgeSourceType::ChangeAnalysis,
        source_ref: source_ref.to_string(),
        importance,
        reasons: reasons_for_asset(asset, &related_change_types),
        recommended_doc: asset.asset.clone(),
        related_files: asset.related_files.clone(),
        confidence,
        reviewed_by_model: false,
        model_recommendation: None,
        model_rationale: None,
        status: KnowledgeStatus::Pending,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn confidence_for_importance(importance: &KnowledgeImportance) -> u8 {
    match importance {
        KnowledgeImportance::High => 90,
        KnowledgeImportance::Medium => 75,
        KnowledgeImportance::Low => 55,
    }
}

fn candidate_id(analysis: &ChangeAnalysis, asset: &ImpactedAsset, index: usize) -> String {
    let head = analysis.git_head.as_deref().unwrap_or("nohead");
    let normalized_asset = asset
        .asset
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    let mut hasher = DefaultHasher::new();
    asset.asset.hash(&mut hasher);
    asset.reason.hash(&mut hasher);
    asset.related_files.hash(&mut hasher);
    let change_digest = hasher.finish();

    format!(
        "kc_{}_{}_{:016x}_{}",
        head,
        normalized_asset,
        change_digest,
        index + 1
    )
}

fn summary_for_asset(asset: &ImpactedAsset) -> String {
    format!("建议更新 {}：{}", asset.asset, asset.reason)
}

fn related_change_types(
    analysis: &ChangeAnalysis,
    related_files: &[String],
) -> BTreeSet<ChangeType> {
    let related = related_files.iter().collect::<BTreeSet<_>>();
    let mut change_types = BTreeSet::new();

    for file in &analysis.changed_files {
        if related.contains(&file.path) {
            change_types.extend(file.change_types.iter().cloned());
        }
    }

    change_types
}

fn score_importance(
    related_change_types: &BTreeSet<ChangeType>,
    asset: &ImpactedAsset,
) -> KnowledgeImportance {
    if related_change_types.contains(&ChangeType::Schema)
        || related_change_types.contains(&ChangeType::Api)
        || related_change_types.contains(&ChangeType::Environment)
    {
        return KnowledgeImportance::High;
    }

    if related_change_types.contains(&ChangeType::Dependency)
        || related_change_types.contains(&ChangeType::Config)
        || related_change_types.contains(&ChangeType::Architecture)
        || asset.related_files.len() >= 3
    {
        return KnowledgeImportance::Medium;
    }

    KnowledgeImportance::Low
}

fn reasons_for_asset(
    asset: &ImpactedAsset,
    related_change_types: &BTreeSet<ChangeType>,
) -> Vec<String> {
    let mut reasons = vec![asset.reason.clone()];

    if related_change_types.contains(&ChangeType::Api) {
        reasons.push("接口变化会影响调用方和 API 文档".to_string());
    }
    if related_change_types.contains(&ChangeType::Schema) {
        reasons.push("数据模型变化会影响数据库说明和业务规则".to_string());
    }
    if related_change_types.contains(&ChangeType::Dependency) {
        reasons.push("依赖变化可能影响安装、构建或运行环境".to_string());
    }
    if related_change_types.contains(&ChangeType::Environment) {
        reasons.push("环境变量变化会影响本地启动和部署配置".to_string());
    }
    if related_change_types.contains(&ChangeType::Config) {
        reasons.push("配置变化需要同步到环境说明或部署文档".to_string());
    }
    if related_change_types.contains(&ChangeType::Architecture) {
        reasons.push("模块结构变化需要同步架构说明".to_string());
    }

    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use cyclaw_change_radar::{ChangeSummary, ChangedFile, GitFileStatus, ImpactedAsset};

    #[test]
    fn creates_candidates_from_impacted_assets() {
        let analysis = ChangeAnalysis {
            schema_version: 1,
            project_root: "/tmp/demo".to_string(),
            analyzed_at: "2026-07-06T00:00:00Z".to_string(),
            git_branch: Some("main".to_string()),
            git_head: Some("abc123".to_string()),
            has_changes: true,
            changed_files: vec![ChangedFile {
                path: "package.json".to_string(),
                status: GitFileStatus::Modified,
                change_types: vec![ChangeType::Dependency],
                suggested_docs: vec!["docs/dependencies.md".to_string()],
            }],
            impacted_assets: vec![ImpactedAsset {
                asset: "docs/dependencies.md".to_string(),
                reason: "依赖清单或锁文件发生变化".to_string(),
                related_files: vec!["package.json".to_string()],
            }],
            summary: ChangeSummary {
                total_files: 1,
                api_changes: 0,
                schema_changes: 0,
                dependency_changes: 1,
                config_changes: 0,
                environment_changes: 0,
                documentation_changes: 0,
                architecture_changes: 0,
            },
        };

        let candidates = candidates_from_change_analysis(&analysis, "run/change-analysis.json");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].recommended_doc, "docs/dependencies.md");
        assert_eq!(candidates[0].importance, KnowledgeImportance::Medium);
        assert_eq!(candidates[0].status, KnowledgeStatus::Pending);
        assert_eq!(candidates[0].confidence, 75);
    }

    #[test]
    fn reads_legacy_candidate_without_review_fields() {
        let json = r#"{"id":"legacy","summary":"旧候选","source_type":"manual","source_ref":"manual","importance":"low","reasons":[],"recommended_doc":"docs/changelog.md","related_files":[],"status":"pending","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let candidates = parse_jsonl(json).unwrap();

        assert_eq!(candidates[0].confidence, 50);
        assert!(!candidates[0].reviewed_by_model);
    }
}
