use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectProfile {
    pub schema_version: u32,
    pub project_root: String,
    pub scanned_at: String,
    pub is_git_repository: bool,
    pub git_branch: Option<String>,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub dependency_files: Vec<PathRecord>,
    pub document_paths: Vec<PathRecord>,
    pub config_files: Vec<PathRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathRecord {
    pub path: String,
    pub kind: String,
}

pub fn scan_project_profile(project_root: &Path) -> Result<ProjectProfile> {
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("无法解析项目根目录: {}", project_root.display()))?;

    let files = collect_files(&project_root)?;
    let dependency_files = detect_dependency_files(&project_root, &files);
    let document_paths = detect_document_paths(&project_root, &files);
    let config_files = detect_config_files(&project_root, &files);
    let languages = detect_languages(&files, &dependency_files);
    let frameworks = detect_frameworks(&project_root, &dependency_files);
    let is_git_repository = is_git_repository(&project_root);
    let git_branch = if is_git_repository {
        current_git_branch(&project_root)
    } else {
        None
    };

    Ok(ProjectProfile {
        schema_version: 1,
        project_root: project_root.display().to_string(),
        scanned_at: Utc::now().to_rfc3339(),
        is_git_repository,
        git_branch,
        languages,
        frameworks,
        dependency_files,
        document_paths,
        config_files,
    })
}

fn collect_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    visit_dir(project_root, project_root, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit_dir(project_root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("无法读取目录: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if should_ignore_dir(&file_name) {
                continue;
            }
            visit_dir(project_root, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(project_root)?.to_path_buf();
            files.push(relative);
        }
    }
    Ok(())
}

fn should_ignore_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".cyclaw"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".venv"
            | "venv"
            | "__pycache__"
    )
}

fn detect_dependency_files(project_root: &Path, files: &[PathBuf]) -> Vec<PathRecord> {
    let mut records = Vec::new();

    for file in files {
        let normalized = normalize_path(file);
        let kind = match normalized.as_str() {
            "package.json" => Some("Node.js package manifest"),
            "pnpm-lock.yaml" => Some("pnpm lockfile"),
            "package-lock.json" => Some("npm lockfile"),
            "yarn.lock" => Some("Yarn lockfile"),
            "pom.xml" => Some("Maven project"),
            "build.gradle" | "build.gradle.kts" => Some("Gradle project"),
            "requirements.txt" => Some("Python requirements"),
            "pyproject.toml" => Some("Python project"),
            "go.mod" => Some("Go module"),
            "Cargo.toml" => Some("Rust package manifest"),
            _ => None,
        };

        if let Some(kind) = kind {
            records.push(PathRecord {
                path: normalized,
                kind: kind.to_string(),
            });
        }
    }

    records.extend(detect_nested_package_manifests(project_root, files));
    dedupe_records(records)
}

fn detect_nested_package_manifests(_project_root: &Path, files: &[PathBuf]) -> Vec<PathRecord> {
    files
        .iter()
        .filter_map(|file| {
            let normalized = normalize_path(file);
            if normalized.ends_with("/package.json") && normalized != "package.json" {
                Some(PathRecord {
                    path: normalized,
                    kind: "Nested Node.js package manifest".to_string(),
                })
            } else if normalized.ends_with("/Cargo.toml") && normalized != "Cargo.toml" {
                Some(PathRecord {
                    path: normalized,
                    kind: "Nested Rust package manifest".to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn detect_document_paths(_project_root: &Path, files: &[PathBuf]) -> Vec<PathRecord> {
    let mut records = Vec::new();

    for file in files {
        let normalized = normalize_path(file);
        let lower = normalized.to_lowercase();

        if lower == "readme.md" {
            records.push(PathRecord {
                path: normalized,
                kind: "README".to_string(),
            });
        } else if lower.starts_with("docs/") && lower.ends_with(".md") {
            records.push(PathRecord {
                path: normalized,
                kind: "Markdown documentation".to_string(),
            });
        } else if lower.starts_with("wiki/") && lower.ends_with(".md") {
            records.push(PathRecord {
                path: normalized,
                kind: "Wiki documentation".to_string(),
            });
        }
    }

    dedupe_records(records)
}

fn detect_config_files(_project_root: &Path, files: &[PathBuf]) -> Vec<PathRecord> {
    let mut records = Vec::new();

    for file in files {
        let normalized = normalize_path(file);
        let file_name = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        let kind = if matches!(
            normalized.as_str(),
            ".env.example" | ".env.sample" | ".env.template"
        ) {
            Some("Environment template")
        } else if file_name.starts_with("config.") {
            Some("Application config")
        } else if matches!(
            file_name,
            "application.yml" | "application.yaml" | "application.properties"
        ) {
            Some("Spring application config")
        } else if matches!(
            file_name,
            "tsconfig.json" | "vite.config.ts" | "vite.config.js"
        ) {
            Some("Frontend build config")
        } else {
            None
        };

        if let Some(kind) = kind {
            records.push(PathRecord {
                path: normalized,
                kind: kind.to_string(),
            });
        }
    }

    dedupe_records(records)
}

fn detect_languages(files: &[PathBuf], dependency_files: &[PathRecord]) -> Vec<String> {
    let mut languages = BTreeSet::new();

    for record in dependency_files {
        match record.path.as_str() {
            "package.json" | "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock" => {
                languages.insert("JavaScript/TypeScript".to_string());
            }
            "Cargo.toml" => {
                languages.insert("Rust".to_string());
            }
            "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                languages.insert("Java/Kotlin".to_string());
            }
            "requirements.txt" | "pyproject.toml" => {
                languages.insert("Python".to_string());
            }
            "go.mod" => {
                languages.insert("Go".to_string());
            }
            _ => {}
        }
    }

    for file in files {
        match file.extension().and_then(|extension| extension.to_str()) {
            Some("rs") => {
                languages.insert("Rust".to_string());
            }
            Some("ts") | Some("tsx") => {
                languages.insert("TypeScript".to_string());
            }
            Some("js") | Some("jsx") => {
                languages.insert("JavaScript".to_string());
            }
            Some("java") | Some("kt") => {
                languages.insert("Java/Kotlin".to_string());
            }
            Some("py") => {
                languages.insert("Python".to_string());
            }
            Some("go") => {
                languages.insert("Go".to_string());
            }
            _ => {}
        }
    }

    languages.into_iter().collect()
}

fn detect_frameworks(project_root: &Path, dependency_files: &[PathRecord]) -> Vec<String> {
    let mut frameworks = BTreeSet::new();

    for record in dependency_files {
        let path = project_root.join(&record.path);
        match record.path.as_str() {
            "package.json" => detect_node_frameworks(&path, &mut frameworks),
            "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                detect_text_markers(
                    &path,
                    &mut frameworks,
                    &[
                        ("spring-boot", "Spring Boot"),
                        ("org.springframework", "Spring"),
                    ],
                );
            }
            "requirements.txt" | "pyproject.toml" => {
                detect_text_markers(
                    &path,
                    &mut frameworks,
                    &[
                        ("fastapi", "FastAPI"),
                        ("django", "Django"),
                        ("flask", "Flask"),
                    ],
                );
            }
            "Cargo.toml" => {
                detect_text_markers(
                    &path,
                    &mut frameworks,
                    &[
                        ("tauri", "Tauri"),
                        ("axum", "Axum"),
                        ("actix-web", "Actix Web"),
                    ],
                );
            }
            _ => {}
        }
    }

    frameworks.into_iter().collect()
}

fn detect_node_frameworks(path: &Path, frameworks: &mut BTreeSet<String>) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };

    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };

    for section in ["dependencies", "devDependencies"] {
        let Some(deps) = json.get(section).and_then(|value| value.as_object()) else {
            continue;
        };

        for name in deps.keys() {
            match name.as_str() {
                "react" => {
                    frameworks.insert("React".to_string());
                }
                "next" => {
                    frameworks.insert("Next.js".to_string());
                }
                "vue" => {
                    frameworks.insert("Vue".to_string());
                }
                "vite" => {
                    frameworks.insert("Vite".to_string());
                }
                "express" => {
                    frameworks.insert("Express".to_string());
                }
                "nestjs" | "@nestjs/core" => {
                    frameworks.insert("NestJS".to_string());
                }
                "svelte" => {
                    frameworks.insert("Svelte".to_string());
                }
                _ => {}
            }
        }
    }
}

fn detect_text_markers(path: &Path, frameworks: &mut BTreeSet<String>, markers: &[(&str, &str)]) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    let lower = content.to_lowercase();

    for (marker, framework) in markers {
        if lower.contains(&marker.to_lowercase()) {
            frameworks.insert((*framework).to_string());
        }
    }
}

fn is_git_repository(project_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn current_git_branch(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn dedupe_records(records: Vec<PathRecord>) -> Vec<PathRecord> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();

    for record in records {
        if seen.insert(record.path.clone()) {
            deduped.push(record);
        }
    }

    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_detects_node_project() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("package.json"),
            r#"{"dependencies":{"react":"latest","vite":"latest"}}"#,
        )
        .unwrap();
        fs::write(temp.path().join("README.md"), "# Demo").unwrap();
        fs::write(temp.path().join(".env.example"), "API_URL=").unwrap();

        let profile = scan_project_profile(temp.path()).unwrap();

        assert!(
            profile
                .languages
                .contains(&"JavaScript/TypeScript".to_string())
        );
        assert!(profile.frameworks.contains(&"React".to_string()));
        assert!(profile.frameworks.contains(&"Vite".to_string()));
        assert_eq!(profile.dependency_files.len(), 1);
        assert_eq!(profile.document_paths.len(), 1);
        assert_eq!(profile.config_files.len(), 1);
    }

    #[test]
    fn scan_ignores_target_and_node_modules() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("node_modules/pkg")).unwrap();
        fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"demo\"").unwrap();
        fs::write(temp.path().join("node_modules/pkg/package.json"), "{}").unwrap();
        fs::write(temp.path().join("target/debug/file.rs"), "fn main() {}").unwrap();

        let profile = scan_project_profile(temp.path()).unwrap();

        assert_eq!(profile.dependency_files.len(), 1);
        assert_eq!(profile.dependency_files[0].path, "Cargo.toml");
    }
}
