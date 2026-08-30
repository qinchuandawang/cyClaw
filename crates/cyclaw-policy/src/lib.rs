use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CYCLE_DIR: &str = ".cyclaw";
const CONFIG_FILE: &str = "config.yaml";
const LOCK_DIR: &str = "locks";

/// 项目级进程锁，使用独占文件避免 watch、Agent 和插件同时覆盖同一份产物。
pub struct ProjectLock {
    path: PathBuf,
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn acquire_lock(project_root: &Path, name: &str, timeout: Duration) -> Result<ProjectLock> {
    let lock_dir = project_root.join(CYCLE_DIR).join(LOCK_DIR);
    fs::create_dir_all(&lock_dir)
        .with_context(|| format!("无法创建锁目录: {}", lock_dir.display()))?;
    let path = lock_dir.join(format!("{}.lock", sanitize_lock_name(name)));
    let deadline = Instant::now() + timeout;

    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                writeln!(file, "{}", std::process::id())?;
                return Ok(ProjectLock { path });
            }
            Err(error)
                if error.kind() == ErrorKind::AlreadyExists
                    || (error.kind() == ErrorKind::PermissionDenied && path.exists()) =>
            {
                if lock_is_stale(&path) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                // Windows 可能将已存在锁文件的 create_new 竞争报告为 PermissionDenied。
                if Instant::now() >= deadline {
                    anyhow::bail!("获取项目锁超时: {}", path.display());
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("无法创建项目锁: {}", path.display()));
            }
        }
    }
}

pub fn lock_exists(project_root: &Path, name: &str) -> bool {
    let path = project_root
        .join(CYCLE_DIR)
        .join(LOCK_DIR)
        .join(format!("{}.lock", sanitize_lock_name(name)));
    path.exists() && !lock_is_stale(&path)
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return true;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return true;
    };
    if pid == std::process::id() {
        return false;
    }
    !process_is_running(pid)
}

fn process_is_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {}", pid);
        std::process::Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .ok()
            .is_some_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
            })
    }
    #[cfg(unix)]
    {
        return std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        true
    }
}

fn sanitize_lock_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    ReadOnly,
    LocalKnowledgeWrite,
    DocsWrite,
    ModelCall,
    Automation,
    Shell,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyConfig {
    pub schema_version: u32,
    pub permissions: PermissionConfig,
    pub model_policy: ModelPolicy,
    pub automation: AutomationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionConfig {
    pub default_level: PermissionLevel,
    pub read_scope: Vec<String>,
    pub write_scopes: Vec<String>,
    pub denied_paths: Vec<String>,
    pub allow_model_call: bool,
    pub allow_network: bool,
    pub allow_docs_apply: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelPolicy {
    pub max_context_chars: usize,
    pub allow_source_snippets: bool,
    pub allow_config_snippets: bool,
    pub redact_secrets: bool,
    pub max_concurrent_calls: usize,
    pub request_timeout_seconds: u64,
    pub max_retries: usize,
    pub min_interval_millis: u64,
    #[serde(default)]
    pub cache_model_responses: bool,
    #[serde(default = "default_model_input_tokens")]
    pub max_input_tokens: usize,
    #[serde(default = "default_model_output_tokens")]
    pub max_output_tokens: usize,
    #[serde(default = "default_daily_model_tokens")]
    pub daily_token_budget: usize,
}

fn default_model_input_tokens() -> usize {
    12000
}
fn default_model_output_tokens() -> usize {
    512
}
fn default_daily_model_tokens() -> usize {
    100000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationPolicy {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyCheck {
    pub allowed: bool,
    pub reason: String,
    pub normalized_path: String,
}

pub fn policy_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(CONFIG_FILE)
}

pub fn load_or_default(project_root: &Path) -> Result<PolicyConfig> {
    let path = policy_path(project_root);
    if !path.exists() {
        return Ok(default_policy());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取配置文件: {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    Ok(policy_from_yaml(value))
}

pub fn set_permission(project_root: &Path, key: &str, enabled: bool) -> Result<PolicyConfig> {
    let _lock = acquire_lock(project_root, "policy", Duration::from_secs(5))?;
    let path = policy_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut value = if path.exists() {
        serde_yaml::from_str::<serde_yaml::Value>(&fs::read_to_string(&path)?)?
    } else {
        serde_yaml::to_value(default_policy())?
    };
    let mapping = value
        .as_mapping_mut()
        .with_context(|| "cyClaw 配置不是 YAML 对象")?;
    let permissions_key = serde_yaml::Value::String("permissions".to_string());
    let permissions = mapping
        .entry(permissions_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()))
        .as_mapping_mut()
        .with_context(|| "permissions 不是 YAML 对象")?;
    let allowed = ["allow_model_call", "allow_network", "allow_docs_apply"];
    if !allowed.contains(&key) {
        anyhow::bail!("不支持的权限开关: {}", key);
    }
    permissions.insert(
        serde_yaml::Value::String(key.to_string()),
        serde_yaml::Value::Bool(enabled),
    );
    fs::write(&path, serde_yaml::to_string(&value)?)?;
    load_or_default(project_root)
}

pub fn check_write_path(
    project_root: &Path,
    target: &str,
    level: PermissionLevel,
) -> Result<PolicyCheck> {
    let policy = load_or_default(project_root)?;
    Ok(check_write_path_with_policy(&policy, target, level))
}

pub fn check_model_call(project_root: &Path, level: PermissionLevel) -> Result<PolicyCheck> {
    let policy = load_or_default(project_root)?;
    let allowed = level >= PermissionLevel::ModelCall && policy.permissions.allow_model_call;
    Ok(PolicyCheck {
        allowed,
        reason: if allowed {
            "允许模型调用".to_string()
        } else {
            "模型调用未授权".to_string()
        },
        normalized_path: "model_call".to_string(),
    })
}

pub fn check_write_path_with_policy(
    policy: &PolicyConfig,
    target: &str,
    level: PermissionLevel,
) -> PolicyCheck {
    let normalized = normalize_path(target);

    if normalized.contains("..") {
        return denied(normalized, "路径包含 ..");
    }

    if is_denied_path(policy, &normalized) {
        return denied(normalized, "路径命中 denied_paths");
    }

    if normalized.starts_with("docs/") && level < PermissionLevel::DocsWrite {
        return denied(normalized, "写 docs 需要 DocsWrite 权限");
    }

    let allowed = policy
        .permissions
        .write_scopes
        .iter()
        .any(|scope| path_in_scope(&normalized, scope));

    if allowed {
        PolicyCheck {
            allowed: true,
            reason: "路径在允许写入范围内".to_string(),
            normalized_path: normalized,
        }
    } else {
        denied(normalized, "路径不在允许写入范围内")
    }
}

pub fn default_policy() -> PolicyConfig {
    PolicyConfig {
        schema_version: 1,
        permissions: PermissionConfig {
            default_level: PermissionLevel::LocalKnowledgeWrite,
            read_scope: vec![".".to_string()],
            write_scopes: vec![".cyclaw".to_string(), "docs".to_string()],
            denied_paths: vec![
                ".env".to_string(),
                ".env.*".to_string(),
                "**/*.pem".to_string(),
                "**/*.key".to_string(),
                "**/id_rsa".to_string(),
                ".git".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
            ],
            allow_model_call: false,
            allow_network: false,
            allow_docs_apply: false,
        },
        model_policy: ModelPolicy {
            max_context_chars: 30000,
            allow_source_snippets: false,
            allow_config_snippets: true,
            redact_secrets: true,
            max_concurrent_calls: 1,
            request_timeout_seconds: 30,
            max_retries: 2,
            min_interval_millis: 100,
            cache_model_responses: false,
            max_input_tokens: default_model_input_tokens(),
            max_output_tokens: default_model_output_tokens(),
            daily_token_budget: default_daily_model_tokens(),
        },
        automation: AutomationPolicy {},
    }
}

fn policy_from_yaml(value: serde_yaml::Value) -> PolicyConfig {
    let mut policy = default_policy();
    if let Some(permissions) = value.get("permissions") {
        if let Some(write_scopes) = permissions
            .get("write_scopes")
            .and_then(|value| value.as_sequence())
        {
            policy.permissions.write_scopes = write_scopes
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect();
        }
        if let Some(allow_model_call) = permissions
            .get("allow_model_call")
            .and_then(|value| value.as_bool())
        {
            policy.permissions.allow_model_call = allow_model_call;
        }
        if let Some(allow_network) = permissions
            .get("allow_network")
            .and_then(|value| value.as_bool())
        {
            policy.permissions.allow_network = allow_network;
        }
        if let Some(allow_docs_apply) = permissions
            .get("allow_docs_apply")
            .and_then(|value| value.as_bool())
        {
            policy.permissions.allow_docs_apply = allow_docs_apply;
        }
    }
    if let Some(model_policy) = value.get("model_policy") {
        if let Some(value) = model_policy
            .get("max_context_chars")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.max_context_chars = value as usize;
        }
        if let Some(value) = model_policy
            .get("allow_source_snippets")
            .and_then(|value| value.as_bool())
        {
            policy.model_policy.allow_source_snippets = value;
        }
        if let Some(value) = model_policy
            .get("allow_config_snippets")
            .and_then(|value| value.as_bool())
        {
            policy.model_policy.allow_config_snippets = value;
        }
        if let Some(value) = model_policy
            .get("redact_secrets")
            .and_then(|value| value.as_bool())
        {
            policy.model_policy.redact_secrets = value;
        }
        if let Some(value) = model_policy
            .get("max_concurrent_calls")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.max_concurrent_calls = value.max(1) as usize;
        }
        if let Some(value) = model_policy
            .get("request_timeout_seconds")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.request_timeout_seconds = value.max(1);
        }
        if let Some(value) = model_policy
            .get("max_retries")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.max_retries = value as usize;
        }
        if let Some(value) = model_policy
            .get("min_interval_millis")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.min_interval_millis = value;
        }
        if let Some(value) = model_policy
            .get("cache_model_responses")
            .and_then(|value| value.as_bool())
        {
            policy.model_policy.cache_model_responses = value;
        }
        if let Some(value) = model_policy
            .get("max_input_tokens")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.max_input_tokens = value.max(256) as usize;
        }
        if let Some(value) = model_policy
            .get("max_output_tokens")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.max_output_tokens = value.max(64) as usize;
        }
        if let Some(value) = model_policy
            .get("daily_token_budget")
            .and_then(|value| value.as_u64())
        {
            policy.model_policy.daily_token_budget = value.max(1) as usize;
        }
    }
    policy
}

fn is_denied_path(policy: &PolicyConfig, normalized: &str) -> bool {
    policy.permissions.denied_paths.iter().any(|pattern| {
        normalized == pattern
            || normalized.starts_with(&format!("{}/", pattern.trim_end_matches('/')))
            || (pattern.starts_with("**/*.")
                && normalized.ends_with(pattern.trim_start_matches("**/*")))
            || (pattern.ends_with(".*") && normalized.starts_with(pattern.trim_end_matches(".*")))
    })
}

fn path_in_scope(path: &str, scope: &str) -> bool {
    let normalized_scope = normalize_path(scope).trim_end_matches('/').to_string();
    path == normalized_scope || path.starts_with(&format!("{}/", normalized_scope))
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn denied(normalized_path: String, reason: &str) -> PolicyCheck {
    PolicyCheck {
        allowed: false,
        reason: reason.to_string(),
        normalized_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_cyclaw_writes() {
        let policy = default_policy();
        let result = check_write_path_with_policy(
            &policy,
            ".cyclaw/agent-runs/run.json",
            PermissionLevel::LocalKnowledgeWrite,
        );

        assert!(result.allowed);
    }

    #[test]
    fn docs_write_requires_docs_level() {
        let policy = default_policy();
        let result = check_write_path_with_policy(
            &policy,
            "docs/api.md",
            PermissionLevel::LocalKnowledgeWrite,
        );

        assert!(!result.allowed);
    }

    #[test]
    fn denied_paths_are_blocked() {
        let policy = default_policy();
        let result = check_write_path_with_policy(&policy, ".env", PermissionLevel::DocsWrite);

        assert!(!result.allowed);
    }
}
