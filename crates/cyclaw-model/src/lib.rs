use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cyclaw_policy::{PermissionLevel, acquire_lock, check_model_call, load_or_default};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::NamedTempFile;

const CYCLE_DIR: &str = ".cyclaw";
const MODEL_CONFIG_FILE: &str = "model-providers.yaml";
const MODEL_CACHE_FILE: &str = "cache/model-cache.json";

#[derive(Debug, Clone)]
pub struct AddProviderOptions {
    pub project_root: PathBuf,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub thinking_enabled: bool,
    pub set_active: bool,
}

#[derive(Debug, Clone)]
pub struct TestProviderOptions {
    pub project_root: PathBuf,
    pub name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProvidersConfig {
    pub schema_version: u32,
    pub active_provider: Option<String>,
    pub providers: BTreeMap<String, ModelProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProviderConfig {
    pub provider_type: ModelProviderType,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub thinking_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderType {
    OpenAiCompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddProviderResult {
    pub config_path: PathBuf,
    pub provider: ModelProviderConfig,
    pub active_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListProvidersResult {
    pub config_path: PathBuf,
    pub active_provider: Option<String>,
    pub providers: BTreeMap<String, ModelProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestProviderResult {
    pub provider_name: String,
    pub model: String,
    pub response: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub latency_millis: u128,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelUsage {
    pub date: String,
    pub calls: usize,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UseProviderResult {
    pub config_path: PathBuf,
    pub active_provider: String,
}

pub fn add_provider(options: AddProviderOptions) -> Result<AddProviderResult> {
    ensure_directory(&options.project_root)?;
    let config_path = config_path(&options.project_root);
    let mut config = read_config(&options.project_root)?;
    let provider = ModelProviderConfig {
        provider_type: ModelProviderType::OpenAiCompatible,
        base_url: normalize_base_url(&options.base_url),
        model: options.model,
        api_key_env: options.api_key_env,
        thinking_enabled: options.thinking_enabled,
    };

    config
        .providers
        .insert(options.name.clone(), provider.clone());
    if options.set_active || config.active_provider.is_none() {
        config.active_provider = Some(options.name);
    }

    write_config(&config_path, &config)?;
    enable_model_permissions(&options.project_root)?;

    Ok(AddProviderResult {
        config_path,
        provider,
        active_provider: config.active_provider,
    })
}

pub fn list_providers(project_root: PathBuf) -> Result<ListProvidersResult> {
    ensure_directory(&project_root)?;
    let config_path = config_path(&project_root);
    let config = read_config(&project_root)?;

    Ok(ListProvidersResult {
        config_path,
        active_provider: config.active_provider,
        providers: config.providers,
    })
}

pub fn use_provider(project_root: PathBuf, name: &str) -> Result<UseProviderResult> {
    ensure_directory(&project_root)?;
    let config_path = config_path(&project_root);
    let mut config = read_config(&project_root)?;
    if !config.providers.contains_key(name) {
        anyhow::bail!("未找到模型 Provider: {}", name);
    }
    config.active_provider = Some(name.to_string());
    write_config(&config_path, &config)?;
    Ok(UseProviderResult {
        config_path,
        active_provider: name.to_string(),
    })
}

pub fn test_provider(options: TestProviderOptions) -> Result<TestProviderResult> {
    ensure_directory(&options.project_root)?;
    let policy = load_or_default(&options.project_root)?;
    let _lock = acquire_model_slot(
        &options.project_root,
        policy.model_policy.max_concurrent_calls,
        Duration::from_secs(policy.model_policy.request_timeout_seconds.max(5)),
    )?;
    let policy_check = check_model_call(&options.project_root, PermissionLevel::ModelCall)?;
    if !policy_check.allowed {
        anyhow::bail!("权限拒绝模型调用: {}", policy_check.reason);
    }
    let config = read_config(&options.project_root)?;
    let Some(provider) = config.providers.get(&options.name) else {
        anyhow::bail!("未找到模型 Provider: {}", options.name);
    };

    let prompt = if policy.model_policy.redact_secrets {
        redact_sensitive_text(&options.prompt)
    } else {
        options.prompt.clone()
    };
    let cache_key = cache_key(&options.name, provider, &prompt);
    if policy.model_policy.cache_model_responses
        && let Some(response) = read_cached_response(&options.project_root, &cache_key)?
    {
        return Ok(TestProviderResult {
            provider_name: options.name,
            model: provider.model.clone(),
            response,
            input_tokens: estimate_tokens(&prompt),
            output_tokens: 0,
            total_tokens: estimate_tokens(&prompt),
            latency_millis: 0,
            cache_hit: true,
        });
    }

    let estimated_input = estimate_tokens(&prompt);
    if estimated_input > policy.model_policy.max_input_tokens {
        anyhow::bail!(
            "模型输入超过上限: {} > {} tokens",
            estimated_input,
            policy.model_policy.max_input_tokens
        );
    }
    reserve_daily_budget(&options.project_root, &policy, estimated_input)?;

    let api_key = std::env::var(&provider.api_key_env)
        .with_context(|| format!("环境变量未设置: {}", provider.api_key_env))?;
    enforce_min_interval(
        &options.project_root,
        policy.model_policy.min_interval_millis,
    )?;
    let started = Instant::now();
    let completion = test_openai_compatible(
        provider,
        &api_key,
        &prompt,
        Duration::from_secs(policy.model_policy.request_timeout_seconds.max(1)),
        policy.model_policy.max_retries,
        policy.model_policy.max_output_tokens,
    )?;
    let latency_millis = started.elapsed().as_millis();
    let input_tokens = completion.input_tokens.unwrap_or(estimated_input);
    let output_tokens = completion
        .output_tokens
        .unwrap_or_else(|| estimate_tokens(&completion.response));
    let total_tokens = completion
        .total_tokens
        .unwrap_or(input_tokens.saturating_add(output_tokens));
    record_usage(
        &options.project_root,
        input_tokens,
        output_tokens,
        total_tokens,
    )?;
    if policy.model_policy.cache_model_responses {
        write_cached_response(&options.project_root, &cache_key, &completion.response)?;
    }

    Ok(TestProviderResult {
        provider_name: options.name,
        model: provider.model.clone(),
        response: completion.response,
        input_tokens,
        output_tokens,
        total_tokens,
        latency_millis,
        cache_hit: false,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelCacheEntry {
    key: String,
    created_at: String,
    response: String,
}

fn cache_key(name: &str, provider: &ModelProviderConfig, prompt: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    provider.base_url.hash(&mut hasher);
    provider.model.hash(&mut hasher);
    provider.thinking_enabled.hash(&mut hasher);
    prompt.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn cache_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(MODEL_CACHE_FILE)
}

fn read_cached_response(project_root: &Path, key: &str) -> Result<Option<String>> {
    let path = cache_path(project_root);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取模型缓存: {}", path.display()))?;
    let entries: Vec<ModelCacheEntry> = serde_json::from_str(&content).unwrap_or_default();
    Ok(entries
        .into_iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.response))
}

fn write_cached_response(project_root: &Path, key: &str, response: &str) -> Result<()> {
    let path = cache_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut entries: Vec<ModelCacheEntry> = if path.exists() {
        serde_json::from_str(&fs::read_to_string(&path)?).unwrap_or_default()
    } else {
        Vec::new()
    };
    entries.retain(|entry| entry.key != key);
    entries.push(ModelCacheEntry {
        key: key.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        response: response.to_string(),
    });
    const MAX_ENTRIES: usize = 128;
    if entries.len() > MAX_ENTRIES {
        let keep_from = entries.len() - MAX_ENTRIES;
        entries.drain(0..keep_from);
    }
    let parent = path.parent().context("模型缓存缺少父目录")?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, &entries)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换模型缓存: {}", path.display()))
        .map(|_| ())
}

pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join(MODEL_CONFIG_FILE)
}

fn read_config(project_root: &Path) -> Result<ModelProvidersConfig> {
    let path = config_path(project_root);
    if !path.exists() {
        return Ok(default_config());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("无法读取模型配置: {}", path.display()))?;
    Ok(serde_yaml::from_str(&content)?)
}

fn write_config(path: &Path, config: &ModelProvidersConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建模型配置目录: {}", parent.display()))?;
    }

    let content = serde_yaml::to_string(config)?;
    fs::write(path, content).with_context(|| format!("无法写入模型配置: {}", path.display()))
}

fn enable_model_permissions(project_root: &Path) -> Result<()> {
    let config_path = project_root.join(CYCLE_DIR).join("config.yaml");
    if !config_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("无法读取 cyClaw 配置: {}", config_path.display()))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    let mapping = value
        .as_mapping_mut()
        .with_context(|| format!("cyClaw 配置不是 YAML 对象: {}", config_path.display()))?;
    let permissions_key = serde_yaml::Value::String("permissions".to_string());
    let permissions = mapping
        .entry(permissions_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let permissions = permissions
        .as_mapping_mut()
        .with_context(|| "permissions 不是 YAML 对象")?;

    permissions.insert(
        serde_yaml::Value::String("allow_model_call".to_string()),
        serde_yaml::Value::Bool(true),
    );
    permissions.insert(
        serde_yaml::Value::String("allow_network".to_string()),
        serde_yaml::Value::Bool(true),
    );

    fs::write(&config_path, serde_yaml::to_string(&value)?)
        .with_context(|| format!("无法更新 cyClaw 配置: {}", config_path.display()))?;
    Ok(())
}

fn default_config() -> ModelProvidersConfig {
    ModelProvidersConfig {
        schema_version: 1,
        active_provider: None,
        providers: BTreeMap::new(),
    }
}

/// 仅在发送外部 Provider 前做保守脱敏；原始项目证据不被写回或覆盖。
fn redact_sensitive_text(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            let contains_secret_name = ["api_key", "apikey", "password", "secret", "token"]
                .iter()
                .any(|name| lower.contains(name));
            if contains_secret_name {
                if let Some(index) = line.find('=') {
                    return format!("{}=<已脱敏>", &line[..index]);
                }
                if let Some(index) = line.find(':') {
                    return format!("{}: <已脱敏>", &line[..index]);
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct CompletionResult {
    response: String,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
}

fn test_openai_compatible(
    provider: &ModelProviderConfig,
    api_key: &str,
    prompt: &str,
    timeout: Duration,
    max_retries: usize,
    max_output_tokens: usize,
) -> Result<CompletionResult> {
    let endpoint = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()?;
    let request_body = json!({
        "model": provider.model,
        "messages": [
            {
                "role": "system",
                "content": "你是 cyClaw 的模型连通性测试助手。请直接回答，不要输出思考过程。"
            },
            {
                "role": "user",
                "content": prompt
            }
        ],
        "temperature": 0,
        "max_tokens": max_output_tokens,
        "thinking": {
            "type": if provider.thinking_enabled { "enabled" } else { "disabled" }
        }
    });
    let mut last_error = "模型请求失败".to_string();
    let mut response = None;
    for attempt in 0..=max_retries {
        match client
            .post(&endpoint)
            .bearer_auth(api_key)
            .json(&request_body)
            .send()
        {
            Ok(candidate) if candidate.status().is_success() => {
                response = Some(candidate);
                break;
            }
            Ok(candidate) => {
                let status = candidate.status();
                let body = candidate.text().unwrap_or_default();
                last_error = format!("HTTP {} {}", status, body);
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < max_retries {
            let backoff = 100_u64.saturating_mul(2_u64.saturating_pow(attempt.min(6) as u32));
            std::thread::sleep(Duration::from_millis(backoff));
        }
    }
    let response = response
        .with_context(|| format!("模型请求失败，已重试 {} 次: {}", max_retries, last_error))?;

    let value: serde_json::Value = response.json().context("模型响应不是合法 JSON")?;

    let message = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"));

    let content = message
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .or_else(|| {
            message
                .and_then(|message| message.get("reasoning_content"))
                .and_then(|content| content.as_str())
        })
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(ToString::to_string)
        .with_context(|| format!("模型响应缺少 choices[0].message.content: {}", value))?;
    let usage = value.get("usage");
    Ok(CompletionResult {
        response: content,
        input_tokens: usage
            .and_then(|v| v.get("prompt_tokens").and_then(|n| n.as_u64()))
            .map(|n| n as usize),
        output_tokens: usage
            .and_then(|v| v.get("completion_tokens").and_then(|n| n.as_u64()))
            .map(|n| n as usize),
        total_tokens: usage
            .and_then(|v| v.get("total_tokens").and_then(|n| n.as_u64()))
            .map(|n| n as usize),
    })
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn usage_path(project_root: &Path) -> PathBuf {
    project_root.join(CYCLE_DIR).join("model-usage.json")
}

fn read_usage(project_root: &Path) -> ModelUsage {
    fs::read_to_string(usage_path(project_root))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .filter(|usage: &ModelUsage| {
            usage.date == chrono::Utc::now().format("%Y-%m-%d").to_string()
        })
        .unwrap_or_else(|| ModelUsage {
            date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
            calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        })
}

pub fn read_model_usage(project_root: &Path) -> ModelUsage {
    read_usage(project_root)
}

fn reserve_daily_budget(
    project_root: &Path,
    policy: &cyclaw_policy::PolicyConfig,
    estimated_input: usize,
) -> Result<()> {
    let _lock = acquire_lock(project_root, "model-usage", Duration::from_secs(5))?;
    let usage = read_usage(project_root);
    let projected = usage
        .total_tokens
        .saturating_add(estimated_input)
        .saturating_add(policy.model_policy.max_output_tokens);
    if projected > policy.model_policy.daily_token_budget {
        anyhow::bail!(
            "模型日预算不足: 预计 {} tokens，预算 {} tokens",
            projected,
            policy.model_policy.daily_token_budget
        );
    }
    Ok(())
}

fn record_usage(
    project_root: &Path,
    input_tokens: usize,
    output_tokens: usize,
    total_tokens: usize,
) -> Result<()> {
    let _lock = acquire_lock(project_root, "model-usage", Duration::from_secs(5))?;
    let mut usage = read_usage(project_root);
    usage.calls = usage.calls.saturating_add(1);
    usage.input_tokens = usage.input_tokens.saturating_add(input_tokens);
    usage.output_tokens = usage.output_tokens.saturating_add(output_tokens);
    usage.total_tokens = usage.total_tokens.saturating_add(total_tokens);
    let path = usage_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut temporary, &usage)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

fn acquire_model_slot(
    project_root: &Path,
    max_slots: usize,
    timeout: Duration,
) -> Result<cyclaw_policy::ProjectLock> {
    let slot_count = max_slots.clamp(1, 32);
    let deadline = Instant::now() + timeout;
    loop {
        for slot in 0..slot_count {
            if let Ok(lock) = acquire_lock(
                project_root,
                &format!("model-{}", slot),
                Duration::from_millis(1),
            ) {
                return Ok(lock);
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("模型并发队列等待超时，当前并发槽位: {}", slot_count);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn enforce_min_interval(project_root: &Path, interval_millis: u64) -> Result<()> {
    if interval_millis == 0 {
        return Ok(());
    }
    let path = project_root.join(CYCLE_DIR).join("model-rate.json");
    let now = std::time::SystemTime::now();
    if let Ok(content) = fs::read_to_string(&path)
        && let Ok(last_millis) = content.trim().parse::<u128>()
    {
        let current = now.duration_since(std::time::UNIX_EPOCH)?.as_millis();
        let elapsed = current.saturating_sub(last_millis);
        if elapsed < interval_millis as u128 {
            std::thread::sleep(Duration::from_millis(
                (interval_millis as u128 - elapsed) as u64,
            ));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = now.duration_since(std::time::UNIX_EPOCH)?.as_millis();
    fs::write(path, current.to_string())?;
    Ok(())
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!("项目根目录不存在或不是目录: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_provider_does_not_store_api_key() {
        let temp = tempfile::tempdir().unwrap();
        let result = add_provider(AddProviderOptions {
            project_root: temp.path().to_path_buf(),
            name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/".to_string(),
            model: "deepseek-v4-flash".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            thinking_enabled: false,
            set_active: true,
        })
        .unwrap();

        let content = fs::read_to_string(result.config_path).unwrap();
        assert!(content.contains("DEEPSEEK_API_KEY"));
        assert!(!content.contains("sk-"));
        assert!(content.contains("thinking_enabled: false"));
        assert_eq!(result.provider.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn use_provider_switches_active_provider() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            add_provider(AddProviderOptions {
                project_root: temp.path().to_path_buf(),
                name: name.to_string(),
                base_url: "https://example.com".to_string(),
                model: "example".to_string(),
                api_key_env: "EXAMPLE_API_KEY".to_string(),
                thinking_enabled: false,
                set_active: name == "first",
            })
            .unwrap();
        }

        let result = use_provider(temp.path().to_path_buf(), "second").unwrap();
        assert_eq!(result.active_provider, "second");
        assert_eq!(
            list_providers(temp.path().to_path_buf())
                .unwrap()
                .active_provider,
            Some("second".to_string())
        );
    }

    #[test]
    fn estimates_tokens_with_a_conservative_lower_bound() {
        assert_eq!(estimate_tokens(""), 1);
        assert_eq!(estimate_tokens("12345678"), 2);
    }

    #[test]
    fn persists_daily_model_usage() {
        let temp = tempfile::tempdir().unwrap();
        record_usage(temp.path(), 10, 5, 15).unwrap();
        let usage = read_model_usage(temp.path());
        assert_eq!(usage.calls, 1);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }
}
