use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexDocument {
    pub source_type: SourceType,
    pub path: String,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Markdown,
    ProjectMemory,
    KnowledgeInbox,
    DocumentPatch,
    ChangeAnalysis,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexSummary {
    pub index_path: PathBuf,
    pub document_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub source_type: SourceType,
    pub path: String,
    pub title: String,
    pub snippet: String,
}

pub fn build_index(project_root: &Path, index_path: &Path) -> Result<IndexSummary> {
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建索引目录: {}", parent.display()))?;
    }

    let documents = collect_documents(project_root)?;
    let mut connection = Connection::open(index_path)
        .with_context(|| format!("无法打开索引数据库: {}", index_path.display()))?;
    initialize_schema(&connection)?;

    let mut existing = HashMap::new();
    {
        let mut statement =
            connection.prepare("SELECT id, source_type, path, title, content FROM documents")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, source_type, path, title, content) = row?;
            existing.insert(path, (id, source_type, title, content));
        }
    }

    let current_paths = documents
        .iter()
        .map(|document| document.path.clone())
        .collect::<HashSet<_>>();
    let transaction = connection.transaction()?;
    for (path, (id, _, _, _)) in &existing {
        if !current_paths.contains(path) {
            transaction.execute("DELETE FROM search_index WHERE rowid = ?1", params![id])?;
            transaction.execute("DELETE FROM documents WHERE id = ?1", params![id])?;
        }
    }
    let indexed_at = Utc::now().to_rfc3339();
    for document in &documents {
        let source_type = source_type_name(&document.source_type);
        let unchanged = existing
            .get(&document.path)
            .map(|(_, old_type, old_title, old_content)| {
                old_type == source_type
                    && old_title == &document.title
                    && old_content == &document.content
            })
            .unwrap_or(false);
        if unchanged {
            continue;
        }
        if let Some((id, _, _, _)) = existing.get(&document.path) {
            transaction.execute("DELETE FROM search_index WHERE rowid = ?1", params![id])?;
            transaction.execute("DELETE FROM documents WHERE id = ?1", params![id])?;
        }
        transaction.execute(
            "INSERT INTO documents (source_type, path, title, content, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source_type, document.path, document.title, document.content, indexed_at],
        )?;
        let rowid = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO search_index (rowid, title, content, path) VALUES (?1, ?2, ?3, ?4)",
            params![rowid, document.title, document.content, document.path],
        )?;
    }

    transaction.commit()?;

    Ok(IndexSummary {
        index_path: index_path.to_path_buf(),
        document_count: documents.len(),
    })
}

pub fn search_index(index_path: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    let connection = Connection::open(index_path)
        .with_context(|| format!("无法打开索引数据库: {}", index_path.display()))?;
    initialize_schema(&connection)?;

    let limit = limit.max(1);
    let mut results = search_with_fts(&connection, query, limit)?;
    if results.is_empty() {
        results = search_with_like(&connection, query, limit)?;
    }

    Ok(results)
}

fn search_with_fts(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let mut statement = connection.prepare(
        r#"
        SELECT d.source_type, d.path, d.title,
               snippet(search_index, 1, '[', ']', '...', 12) AS snippet
        FROM search_index
        JOIN documents d ON d.id = search_index.rowid
        WHERE search_index MATCH ?1
        ORDER BY rank
        LIMIT ?2
        "#,
    )?;

    let rows = statement.query_map(params![query, limit], |row| {
        Ok(SearchResult {
            source_type: parse_source_type(row.get::<_, String>(0)?.as_str()),
            path: row.get(1)?,
            title: row.get(2)?,
            snippet: row.get(3)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(results)
}

fn search_with_like(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let pattern = format!("%{}%", query);
    let mut statement = connection.prepare(
        r#"
        SELECT source_type, path, title, content
        FROM documents
        WHERE title LIKE ?1 OR content LIKE ?1 OR path LIKE ?1
        ORDER BY id
        LIMIT ?2
        "#,
    )?;

    let rows = statement.query_map(params![pattern, limit], |row| {
        let content: String = row.get(3)?;
        Ok(SearchResult {
            source_type: parse_source_type(row.get::<_, String>(0)?.as_str()),
            path: row.get(1)?,
            title: row.get(2)?,
            snippet: build_like_snippet(&content, query),
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(results)
}

fn build_like_snippet(content: &str, query: &str) -> String {
    let Some(index) = content.find(query) else {
        return content.chars().take(80).collect();
    };

    let start = content[..index]
        .char_indices()
        .rev()
        .nth(20)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    let end = content[index + query.len()..]
        .char_indices()
        .nth(40)
        .map(|(idx, _)| index + query.len() + idx)
        .unwrap_or(content.len());

    format!(
        "{}[{}]{}",
        &content[start..index],
        query,
        &content[index + query.len()..end]
    )
}

pub fn collect_documents(project_root: &Path) -> Result<Vec<IndexDocument>> {
    let mut documents = Vec::new();

    collect_markdown_documents(project_root, &mut documents)?;
    collect_cyclaw_documents(project_root, &mut documents)?;

    Ok(documents)
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS documents (
            id INTEGER PRIMARY KEY,
            source_type TEXT NOT NULL,
            path TEXT NOT NULL,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            indexed_at TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS search_index
        USING fts5(title, content, path, content='documents', content_rowid='id');
        "#,
    )?;
    Ok(())
}

fn collect_markdown_documents(
    project_root: &Path,
    documents: &mut Vec<IndexDocument>,
) -> Result<()> {
    for root in ["docs", "wiki"] {
        let dir = project_root.join(root);
        if dir.exists() {
            collect_markdown_dir(project_root, &dir, documents)?;
        }
    }

    let readme = project_root.join("README.md");
    if readme.exists() {
        push_file(project_root, &readme, SourceType::Markdown, documents)?;
    }

    Ok(())
}

fn collect_markdown_dir(
    project_root: &Path,
    dir: &Path,
    documents: &mut Vec<IndexDocument>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("无法读取目录: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_dir(project_root, &path, documents)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            push_file(project_root, &path, SourceType::Markdown, documents)?;
        }
    }

    Ok(())
}

fn collect_cyclaw_documents(project_root: &Path, documents: &mut Vec<IndexDocument>) -> Result<()> {
    let cyclaw_dir = project_root.join(".cyclaw");
    if !cyclaw_dir.exists() {
        return Ok(());
    }

    for (relative, source_type) in [
        ("project.md", SourceType::ProjectMemory),
        ("memory.md", SourceType::ProjectMemory),
        ("knowledge-inbox.jsonl", SourceType::KnowledgeInbox),
    ] {
        let path = cyclaw_dir.join(relative);
        if path.exists() {
            push_file(project_root, &path, source_type, documents)?;
        }
    }

    let patch_dir = cyclaw_dir.join("doc-patches");
    if patch_dir.exists() {
        collect_json_dir(
            project_root,
            &patch_dir,
            SourceType::DocumentPatch,
            documents,
        )?;
    }

    let runs_dir = cyclaw_dir.join("runs");
    if runs_dir.exists() {
        collect_json_dir(
            project_root,
            &runs_dir,
            SourceType::ChangeAnalysis,
            documents,
        )?;
    }

    Ok(())
}

fn collect_json_dir(
    project_root: &Path,
    dir: &Path,
    source_type: SourceType,
    documents: &mut Vec<IndexDocument>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("无法读取目录: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_dir(project_root, &path, source_type.clone(), documents)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            push_file(project_root, &path, source_type.clone(), documents)?;
        }
    }

    Ok(())
}

fn push_file(
    project_root: &Path,
    path: &Path,
    source_type: SourceType,
    documents: &mut Vec<IndexDocument>,
) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("无法读取文件: {}", path.display()))?;
    let relative = relative_path(project_root, path);
    let title = title_from_content_or_path(&content, &relative);

    documents.push(IndexDocument {
        source_type,
        path: relative,
        title,
        content,
    });

    Ok(())
}

fn relative_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|relative| relative.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

fn title_from_content_or_path(content: &str, path: &str) -> String {
    content
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("# ")
                .map(|title| title.trim().to_string())
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn source_type_name(source_type: &SourceType) -> &'static str {
    match source_type {
        SourceType::Markdown => "markdown",
        SourceType::ProjectMemory => "project_memory",
        SourceType::KnowledgeInbox => "knowledge_inbox",
        SourceType::DocumentPatch => "document_patch",
        SourceType::ChangeAnalysis => "change_analysis",
    }
}

fn parse_source_type(value: &str) -> SourceType {
    match value {
        "markdown" => SourceType::Markdown,
        "project_memory" => SourceType::ProjectMemory,
        "knowledge_inbox" => SourceType::KnowledgeInbox,
        "document_patch" => SourceType::DocumentPatch,
        "change_analysis" => SourceType::ChangeAnalysis,
        _ => SourceType::Markdown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_searches_index() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("docs")).unwrap();
        fs::write(
            temp.path().join("docs").join("api.md"),
            "# API\n\n支付回调必须校验签名。",
        )
        .unwrap();

        let index_path = temp.path().join(".cyclaw").join("index.sqlite");
        let summary = build_index(temp.path(), &index_path).unwrap();
        let results = search_index(&index_path, "支付", 10).unwrap();

        assert_eq!(summary.document_count, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "docs/api.md");
    }

    #[test]
    fn incremental_index_updates_and_removes_documents() {
        let temp = tempfile::tempdir().unwrap();
        let docs = temp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        let first = docs.join("first.md");
        let second = docs.join("second.md");
        fs::write(&first, "# First\nalpha").unwrap();
        fs::write(&second, "# Second\nbeta").unwrap();
        let index_path = temp.path().join(".cyclaw").join("index.sqlite");

        assert_eq!(
            build_index(temp.path(), &index_path)
                .unwrap()
                .document_count,
            2
        );
        fs::write(&first, "# First\ngamma").unwrap();
        fs::remove_file(&second).unwrap();
        let summary = build_index(temp.path(), &index_path).unwrap();

        assert_eq!(summary.document_count, 1);
        assert_eq!(search_index(&index_path, "gamma", 10).unwrap().len(), 1);
        assert!(search_index(&index_path, "beta", 10).unwrap().is_empty());
    }
}
