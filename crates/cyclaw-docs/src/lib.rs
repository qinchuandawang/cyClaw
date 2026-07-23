use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::Utc;
use cyclaw_knowledge::{KnowledgeCandidate, KnowledgeImportance};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeOperation {
    #[default]
    Create,
    Update,
    Merge,
    Supersede,
    Delete,
}

impl KnowledgeOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Merge => "merge",
            Self::Supersede => "supersede",
            Self::Delete => "delete",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Create => "新增",
            Self::Update => "更新",
            Self::Merge => "合并",
            Self::Supersede => "取代",
            Self::Delete => "删除",
        }
    }
}

impl FromStr for KnowledgeOperation {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "merge" => Ok(Self::Merge),
            "supersede" => Ok(Self::Supersede),
            "delete" => Ok(Self::Delete),
            _ => anyhow::bail!("未知知识操作: {}", value),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DocumentPatchOptions {
    pub operation: Option<KnowledgeOperation>,
    pub selector: Option<String>,
    pub source_selectors: Vec<String>,
    pub replacement_content: Option<String>,
    pub delete_target_document: bool,
    pub target_existed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentPatch {
    pub id: String,
    pub candidate_id: String,
    pub target_doc: String,
    #[serde(default)]
    pub operation: KnowledgeOperation,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub source_selectors: Vec<String>,
    #[serde(default = "default_target_existed")]
    pub target_existed: bool,
    #[serde(default)]
    pub delete_target_document: bool,
    pub status: DocumentPatchStatus,
    pub summary: String,
    pub original_content: String,
    pub proposed_content: String,
    pub preview: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_target_existed() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentPatchStatus {
    Pending,
    Applied,
    Reverted,
}

pub fn create_document_patch(
    candidate: &KnowledgeCandidate,
    original_content: &str,
) -> Result<DocumentPatch> {
    create_document_patch_with_options(
        candidate,
        original_content,
        DocumentPatchOptions {
            target_existed: true,
            ..DocumentPatchOptions::default()
        },
    )
}

pub fn create_document_patch_with_options(
    candidate: &KnowledgeCandidate,
    original_content: &str,
    options: DocumentPatchOptions,
) -> Result<DocumentPatch> {
    let explicit_operation = options.operation.clone();
    let selector = options.selector.clone().or_else(|| {
        if explicit_operation.is_none() {
            Some(candidate.summary.clone())
        } else {
            None
        }
    });
    let operation = explicit_operation.unwrap_or_else(|| {
        if selector
            .as_deref()
            .and_then(|value| find_section(original_content, value).ok())
            .is_some()
        {
            KnowledgeOperation::Update
        } else {
            KnowledgeOperation::Create
        }
    });
    if options.delete_target_document && operation != KnowledgeOperation::Delete {
        anyhow::bail!("delete_target_document 只能与 delete 操作同时使用");
    }
    let replacement = options
        .replacement_content
        .as_deref()
        .map(normalize_replacement)
        .unwrap_or_else(|| render_candidate_section(candidate));
    let proposed_content = transform_document(
        original_content,
        candidate,
        &operation,
        selector.as_deref(),
        &options.source_selectors,
        &replacement,
        options.delete_target_document,
    )?;
    let now = Utc::now().to_rfc3339();

    Ok(DocumentPatch {
        id: patch_id(
            candidate,
            &operation,
            selector.as_deref(),
            &options.source_selectors,
        ),
        candidate_id: candidate.id.clone(),
        target_doc: candidate.recommended_doc.clone(),
        operation: operation.clone(),
        selector: selector.clone(),
        source_selectors: options.source_selectors.clone(),
        target_existed: options.target_existed,
        delete_target_document: options.delete_target_document,
        status: DocumentPatchStatus::Pending,
        summary: format!("{} {}", operation.label(), candidate.recommended_doc),
        original_content: original_content.to_string(),
        proposed_content,
        preview: render_preview(
            candidate,
            &operation,
            selector.as_deref(),
            &options.source_selectors,
            options.delete_target_document,
        ),
        created_at: now.clone(),
        updated_at: now,
    })
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

pub fn mark_reverted(patch: &mut DocumentPatch) {
    patch.status = DocumentPatchStatus::Reverted;
    patch.updated_at = Utc::now().to_rfc3339();
}

fn transform_document(
    original: &str,
    candidate: &KnowledgeCandidate,
    operation: &KnowledgeOperation,
    selector: Option<&str>,
    source_selectors: &[String],
    replacement: &str,
    delete_target_document: bool,
) -> Result<String> {
    match operation {
        KnowledgeOperation::Create => Ok(append_section(original, replacement)),
        KnowledgeOperation::Update => {
            let selector = selector.context("update 操作需要 selector")?;
            let section = find_section(original, selector)?;
            Ok(replace_ranges(
                original,
                &[(section.range, replacement.to_string())],
            ))
        }
        KnowledgeOperation::Merge => {
            let mut selectors = source_selectors.to_vec();
            if let Some(selector) = selector
                && !selectors.iter().any(|value| value == selector)
            {
                selectors.insert(0, selector.to_string());
            }
            selectors.sort();
            selectors.dedup();
            if selectors.len() < 2 {
                anyhow::bail!("merge 操作至少需要两个不同的章节选择器");
            }
            let mut sections = selectors
                .iter()
                .map(|value| find_section(original, value))
                .collect::<Result<Vec<_>>>()?;
            sections.sort_by_key(|section| section.range.start);
            ensure_non_overlapping(&sections)?;
            let replacements = sections
                .into_iter()
                .enumerate()
                .map(|(index, section)| {
                    (
                        section.range,
                        if index == 0 {
                            replacement.to_string()
                        } else {
                            String::new()
                        },
                    )
                })
                .collect::<Vec<_>>();
            Ok(clean_document_spacing(&replace_ranges(
                original,
                &replacements,
            )))
        }
        KnowledgeOperation::Supersede => {
            let selector = selector.context("supersede 操作需要 selector")?;
            let section = find_section(original, selector)?;
            let old = original[section.range.clone()].trim_end();
            let superseded = format!(
                "{}\n\n> 状态：已由候选 `{}` 于 {} 取代。\n\n{}",
                old,
                candidate.id,
                Utc::now().format("%Y-%m-%d"),
                replacement.trim()
            );
            Ok(replace_ranges(original, &[(section.range, superseded)]))
        }
        KnowledgeOperation::Delete => {
            if delete_target_document {
                return Ok(String::new());
            }
            let selector = selector.context("删除章节时需要 selector")?;
            let section = find_section(original, selector)?;
            Ok(clean_document_spacing(&replace_ranges(
                original,
                &[(section.range, String::new())],
            )))
        }
    }
}

#[derive(Debug, Clone)]
struct MarkdownSection {
    title: String,
    level: u8,
    range: Range<usize>,
}

fn markdown_sections(content: &str) -> Vec<MarkdownSection> {
    let parser = Parser::new_ext(content, Options::all()).into_offset_iter();
    let mut headings = Vec::<(String, u8, usize)>::new();
    let mut current: Option<(u8, usize, String)> = None;

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                if let Some((_, _, title)) = current.as_mut() {
                    title.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, title)) = current.take() {
                    headings.push((title.trim().to_string(), level, start));
                }
            }
            _ => {}
        }
    }

    headings
        .iter()
        .enumerate()
        .map(|(index, (title, level, start))| {
            let end = headings
                .iter()
                .skip(index + 1)
                .find(|(_, next_level, _)| next_level <= level)
                .map(|(_, _, next_start)| *next_start)
                .unwrap_or(content.len());
            MarkdownSection {
                title: title.clone(),
                level: *level,
                range: *start..end,
            }
        })
        .collect()
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn find_section(content: &str, selector: &str) -> Result<MarkdownSection> {
    let sections = markdown_sections(content);
    let normalized = normalize_selector(selector);
    let mut matches = if let Some(candidate_id) = selector.strip_prefix("candidate:") {
        sections
            .into_iter()
            .filter(|section| {
                content[section.range.clone()]
                    .contains(&format!("来源候选：`{}`", candidate_id.trim()))
            })
            .collect::<Vec<_>>()
    } else {
        let exact = sections
            .iter()
            .filter(|section| normalize_selector(&section.title) == normalized)
            .cloned()
            .collect::<Vec<_>>();
        if exact.is_empty() {
            sections
                .into_iter()
                .filter(|section| normalize_selector(&section.title).contains(&normalized))
                .collect::<Vec<_>>()
        } else {
            exact
        }
    };

    if matches.len() != 1 {
        anyhow::bail!(
            "章节选择器必须唯一命中，selector={}，匹配数量={}",
            selector,
            matches.len()
        );
    }
    Ok(matches.remove(0))
}

fn ensure_non_overlapping(sections: &[MarkdownSection]) -> Result<()> {
    for pair in sections.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            anyhow::bail!(
                "不能合并互相嵌套的章节: {}(H{}) 与 {}(H{})",
                pair[0].title,
                pair[0].level,
                pair[1].title,
                pair[1].level
            );
        }
    }
    Ok(())
}

fn replace_ranges(original: &str, replacements: &[(Range<usize>, String)]) -> String {
    let mut content = original.to_string();
    let mut ordered = replacements.to_vec();
    ordered.sort_by_key(|item| std::cmp::Reverse(item.0.start));
    for (range, replacement) in ordered {
        content.replace_range(range, replacement.trim_end());
    }
    content
}

fn patch_id(
    candidate: &KnowledgeCandidate,
    operation: &KnowledgeOperation,
    selector: Option<&str>,
    source_selectors: &[String],
) -> String {
    let mut hasher = DefaultHasher::new();
    operation.hash(&mut hasher);
    selector.hash(&mut hasher);
    source_selectors.hash(&mut hasher);
    format!("patch_{}_{:08x}", candidate.id, hasher.finish() as u32)
}

fn append_section(original_content: &str, section: &str) -> String {
    if original_content.contains(section.trim()) {
        return original_content.to_string();
    }
    let mut content = original_content.trim_end().to_string();
    if !content.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(section.trim());
    content.push('\n');
    content
}

fn normalize_replacement(content: &str) -> String {
    format!("{}\n", content.trim())
}

fn normalize_selector(value: &str) -> String {
    value.trim().trim_start_matches('#').trim().to_lowercase()
}

fn clean_document_spacing(content: &str) -> String {
    let mut cleaned = content.to_string();
    while cleaned.contains("\n\n\n") {
        cleaned = cleaned.replace("\n\n\n", "\n\n");
    }
    if cleaned.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n", cleaned.trim_end())
    }
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
        "## {}\n\n来源候选：`{}`\n\n重要性：{}\n\n{}\n\n### 关联文件\n\n{}\n",
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

fn render_preview(
    candidate: &KnowledgeCandidate,
    operation: &KnowledgeOperation,
    selector: Option<&str>,
    source_selectors: &[String],
    delete_target_document: bool,
) -> String {
    format!(
        "目标文档: {}\n知识操作: {}\n候选知识: {}\n章节选择器: {}\n合并来源: {}\n删除整份文档: {}",
        candidate.recommended_doc,
        operation.as_str(),
        candidate.id,
        selector.unwrap_or("无"),
        if source_selectors.is_empty() {
            "无".to_string()
        } else {
            source_selectors.join(", ")
        },
        delete_target_document
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

    fn candidate() -> KnowledgeCandidate {
        KnowledgeCandidate {
            id: "kc_001".to_string(),
            summary: "依赖管理".to_string(),
            source_type: KnowledgeSourceType::ChangeAnalysis,
            source_ref: ".cyclaw/runs/demo/change-analysis.json".to_string(),
            importance: KnowledgeImportance::Medium,
            reasons: vec!["依赖版本发生变化".to_string()],
            recommended_doc: "docs/dependencies.md".to_string(),
            related_files: vec!["Cargo.toml".to_string()],
            confidence: 75,
            reviewed_by_model: false,
            model_recommendation: None,
            model_rationale: None,
            status: KnowledgeStatus::Pending,
            created_at: "2026-07-06T00:00:00Z".to_string(),
            updated_at: "2026-07-06T00:00:00Z".to_string(),
        }
    }

    fn options(operation: KnowledgeOperation) -> DocumentPatchOptions {
        DocumentPatchOptions {
            operation: Some(operation),
            target_existed: true,
            ..DocumentPatchOptions::default()
        }
    }

    #[test]
    fn creates_new_section() {
        let patch = create_document_patch_with_options(
            &candidate(),
            "# Dependencies\n",
            options(KnowledgeOperation::Create),
        )
        .unwrap();
        assert_eq!(patch.operation, KnowledgeOperation::Create);
        assert!(patch.proposed_content.contains("来源候选：`kc_001`"));
    }

    #[test]
    fn automatically_updates_same_heading_instead_of_appending() {
        let patch = create_document_patch(
            &candidate(),
            "# Dependencies\n\n## 依赖管理\n\n来源候选：`old`\n\n旧内容。\n",
        )
        .unwrap();
        assert_eq!(patch.operation, KnowledgeOperation::Update);
        assert_eq!(patch.proposed_content.matches("## 依赖管理").count(), 1);
        assert!(!patch.proposed_content.contains("旧内容"));
    }

    #[test]
    fn reads_legacy_append_patch_as_create() {
        let patch = parse_patch(
            r##"{
              "id":"patch_legacy",
              "candidate_id":"legacy",
              "target_doc":"docs/legacy.md",
              "status":"pending",
              "summary":"更新 docs/legacy.md",
              "original_content":"# Legacy\n",
              "proposed_content":"# Legacy\n\n内容\n",
              "preview":"旧草稿",
              "created_at":"2026-07-01T00:00:00Z",
              "updated_at":"2026-07-01T00:00:00Z"
            }"##,
        )
        .unwrap();
        assert_eq!(patch.operation, KnowledgeOperation::Create);
        assert!(patch.target_existed);
        assert!(!patch.delete_target_document);
    }

    #[test]
    fn updates_existing_section() {
        let mut value = options(KnowledgeOperation::Update);
        value.selector = Some("旧依赖".to_string());
        value.replacement_content = Some("## 新依赖\n\n使用新版。".to_string());
        let patch = create_document_patch_with_options(
            &candidate(),
            "# Dependencies\n\n## 旧依赖\n\n使用旧版。\n\n## 保留\n\n内容。\n",
            value,
        )
        .unwrap();
        assert!(!patch.proposed_content.contains("使用旧版"));
        assert!(patch.proposed_content.contains("使用新版"));
        assert!(patch.proposed_content.contains("## 保留"));
    }

    #[test]
    fn merges_duplicate_sections() {
        let mut value = options(KnowledgeOperation::Merge);
        value.selector = Some("依赖 A".to_string());
        value.source_selectors = vec!["依赖 B".to_string()];
        value.replacement_content = Some("## 统一依赖\n\n合并内容。".to_string());
        let patch = create_document_patch_with_options(
            &candidate(),
            "# Dependencies\n\n## 依赖 A\n\nA。\n\n## 依赖 B\n\nB。\n",
            value,
        )
        .unwrap();
        assert!(patch.proposed_content.contains("## 统一依赖"));
        assert!(!patch.proposed_content.contains("## 依赖 A"));
        assert!(!patch.proposed_content.contains("## 依赖 B"));
    }

    #[test]
    fn supersedes_old_section_with_history() {
        let mut value = options(KnowledgeOperation::Supersede);
        value.selector = Some("旧方案".to_string());
        value.replacement_content = Some("## 新方案\n\n新事实。".to_string());
        let patch = create_document_patch_with_options(
            &candidate(),
            "# ADR\n\n## 旧方案\n\n旧事实。\n",
            value,
        )
        .unwrap();
        assert!(patch.proposed_content.contains("旧事实"));
        assert!(patch.proposed_content.contains("已由候选 `kc_001`"));
        assert!(patch.proposed_content.contains("新事实"));
    }

    #[test]
    fn deletes_section_or_document() {
        let mut section_options = options(KnowledgeOperation::Delete);
        section_options.selector = Some("废弃内容".to_string());
        let section_patch = create_document_patch_with_options(
            &candidate(),
            "# Docs\n\n## 废弃内容\n\n删除。\n\n## 保留内容\n\n保留。\n",
            section_options,
        )
        .unwrap();
        assert!(!section_patch.proposed_content.contains("废弃内容"));
        assert!(section_patch.proposed_content.contains("保留内容"));

        let mut document_options = options(KnowledgeOperation::Delete);
        document_options.delete_target_document = true;
        let document_patch =
            create_document_patch_with_options(&candidate(), "# Docs\n", document_options).unwrap();
        assert!(document_patch.proposed_content.is_empty());
        assert!(document_patch.delete_target_document);
    }
}
