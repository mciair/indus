//! Provider catalog discovery and local provider preferences.

use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const USER_AGENT: &str = concat!("indus/", env!("CARGO_PKG_VERSION"));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    OpenAi,
    Anthropic,
    Gemini,
    Groq,
    OpenRouter,
}

impl ProviderId {
    pub const ALL: [Self; 5] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::Groq,
        Self::OpenRouter,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::Groq => "Groq",
            Self::OpenRouter => "OpenRouter",
        }
    }

    pub const fn base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: ProviderId,
    pub model_id: String,
    pub model_name: String,
    #[serde(default)]
    pub context_window: Option<u64>,
}

#[derive(Default, Deserialize, Serialize)]
struct StoredCatalog {
    #[serde(default)]
    active: Option<ModelSelection>,
    #[serde(default)]
    recent_models: BTreeMap<ProviderId, String>,
    #[serde(default)]
    api_keys: BTreeMap<ProviderId, String>,
    #[serde(default)]
    cached_models: BTreeMap<ProviderId, Vec<ModelRecord>>,
    #[serde(default)]
    reasoning_efforts: BTreeMap<String, String>,
}

pub struct ProviderStore {
    path: Option<PathBuf>,
    catalog: StoredCatalog,
}

impl ProviderStore {
    pub fn load() -> Self {
        let path = catalog_path();
        Self::from_optional_path(path)
    }

    fn from_optional_path(path: Option<PathBuf>) -> Self {
        let catalog = path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, catalog }
    }

    pub fn active_selection(&self) -> Option<&ModelSelection> {
        self.catalog.active.as_ref()
    }

    pub fn recent_model(&self, provider: ProviderId) -> Option<&str> {
        self.catalog
            .recent_models
            .get(&provider)
            .map(String::as_str)
    }

    pub fn api_key(&self, provider: ProviderId) -> Option<&str> {
        self.catalog
            .api_keys
            .get(&provider)
            .map(String::as_str)
            .filter(|key| !key.is_empty())
    }

    pub fn cached_models(&self, provider: ProviderId) -> &[ModelRecord] {
        self.catalog
            .cached_models
            .get(&provider)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn accept_models(
        &mut self,
        provider: ProviderId,
        models: Vec<ModelRecord>,
        validated_key: Option<String>,
    ) -> io::Result<()> {
        if let Some(key) = validated_key {
            self.catalog.api_keys.insert(provider, key);
        }
        self.catalog.cached_models.insert(provider, models);
        self.persist()
    }

    pub fn select_model(&mut self, provider: ProviderId, model: &ModelRecord) -> io::Result<()> {
        self.catalog
            .recent_models
            .insert(provider, model.id.clone());
        self.catalog.active = Some(ModelSelection {
            provider,
            model_id: model.id.clone(),
            model_name: model.name.clone(),
            context_window: model.context_window,
        });
        let key = model_preference_key(provider, &model.id);
        let valid_saved = self
            .catalog
            .reasoning_efforts
            .get(&key)
            .is_some_and(|value| model.reasoning_efforts.contains(value));
        if !valid_saved {
            if let Some(default) = model
                .default_reasoning_effort
                .as_ref()
                .filter(|value| model.reasoning_efforts.contains(value))
            {
                self.catalog.reasoning_efforts.insert(key, default.clone());
            } else {
                self.catalog.reasoning_efforts.remove(&key);
            }
        }
        self.persist()
    }

    pub fn active_reasoning_efforts(&self) -> &[String] {
        let Some(active) = self.catalog.active.as_ref() else {
            return &[];
        };
        self.cached_models(active.provider)
            .iter()
            .find(|model| model.id == active.model_id)
            .map(|model| model.reasoning_efforts.as_slice())
            .unwrap_or_default()
    }

    pub fn active_reasoning_effort(&self) -> Option<&str> {
        let active = self.catalog.active.as_ref()?;
        self.catalog
            .reasoning_efforts
            .get(&model_preference_key(active.provider, &active.model_id))
            .map(String::as_str)
    }

    pub fn set_active_reasoning_effort(&mut self, effort: &str) -> io::Result<()> {
        let active = self
            .catalog
            .active
            .as_ref()
            .ok_or_else(|| io::Error::other("No model is selected"))?;
        let supported = self.active_reasoning_efforts();
        if !supported.iter().any(|value| value == effort) {
            let offered = if supported.is_empty() {
                "none".to_string()
            } else {
                supported.join(", ")
            };
            return Err(io::Error::other(format!(
                "{} does not expose reasoning effort {effort:?}; available: {offered}",
                active.model_name
            )));
        }
        let key = model_preference_key(active.provider, &active.model_id);
        self.catalog
            .reasoning_efforts
            .insert(key, effort.to_string());
        self.persist()
    }

    pub fn active_context_window(&self) -> Option<u64> {
        let active = self.catalog.active.as_ref()?;
        active.context_window.or_else(|| {
            self.cached_models(active.provider)
                .iter()
                .find(|model| model.id == active.model_id)
                .and_then(|model| model.context_window)
        })
    }

    fn persist(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Some(parent) = path.parent() else {
            return Err(io::Error::other("Provider catalog path has no parent"));
        };
        fs::create_dir_all(parent)?;
        secure_directory(parent)?;

        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.catalog).map_err(io::Error::other)?;
        write_private_file(&temporary, &bytes)?;
        fs::rename(&temporary, path)?;
        secure_file(path)
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self::from_optional_path(Some(path))
    }
}

fn model_preference_key(provider: ProviderId, model_id: &str) -> String {
    format!("{provider:?}:{model_id}")
}

fn catalog_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|root| root.join("indus").join("providers.json"))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryErrorKind {
    Authentication,
    Network,
    InvalidResponse,
    Provider,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryError {
    pub kind: DiscoveryErrorKind,
    pub message: String,
}

impl DiscoveryError {
    fn authentication(provider: ProviderId) -> Self {
        Self {
            kind: DiscoveryErrorKind::Authentication,
            message: format!("{} rejected this API key.", provider.name()),
        }
    }

    fn network(message: impl Into<String>) -> Self {
        Self {
            kind: DiscoveryErrorKind::Network,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: DiscoveryErrorKind::InvalidResponse,
            message: message.into(),
        }
    }

    fn provider(message: impl Into<String>) -> Self {
        Self {
            kind: DiscoveryErrorKind::Provider,
            message: message.into(),
        }
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub struct DiscoveryEvent {
    pub provider: ProviderId,
    pub result: Result<Vec<ModelRecord>, DiscoveryError>,
}

pub struct ModelDiscovery {
    sender: Sender<DiscoveryEvent>,
    receiver: Receiver<DiscoveryEvent>,
}

impl ModelDiscovery {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }

    pub fn fetch(&self, provider: ProviderId, api_key: String) {
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = fetch_models(provider, &api_key);
            let _ = sender.send(DiscoveryEvent { provider, result });
        });
    }

    pub fn drain(&self) -> Vec<DiscoveryEvent> {
        self.receiver.try_iter().collect()
    }
}

impl Default for ModelDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

fn fetch_models(provider: ProviderId, api_key: &str) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| {
            DiscoveryError::network(format!("Could not start model discovery: {error}"))
        })?;
    let mut models = match provider {
        ProviderId::OpenAi => fetch_openai_models(&client, provider, api_key)?,
        ProviderId::Anthropic => fetch_anthropic_models(&client, provider, api_key)?,
        ProviderId::Gemini => fetch_gemini_models(&client, provider, api_key)?,
        ProviderId::Groq => fetch_groq_models(&client, provider, api_key)?,
        ProviderId::OpenRouter => fetch_openrouter_models(&client, provider, api_key)?,
    };
    deduplicate(&mut models);
    if models.is_empty() {
        return Err(DiscoveryError::invalid(format!(
            "{} returned no text-generation models.",
            provider.name()
        )));
    }
    Ok(models)
}

fn fetch_openai_models(
    client: &Client,
    provider: ProviderId,
    api_key: &str,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let value = get_json(
        client
            .get(format!("{}/models", provider.base_url()))
            .bearer_auth(api_key),
        provider,
    )?;
    parse_openai_list(&value, false)
}

fn fetch_groq_models(
    client: &Client,
    provider: ProviderId,
    api_key: &str,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let value = get_json(
        client
            .get(format!("{}/models", provider.base_url()))
            .bearer_auth(api_key),
        provider,
    )?;
    parse_openai_list(&value, true)
}

fn parse_openai_list(
    value: &Value,
    honor_active: bool,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| DiscoveryError::invalid("The provider returned an invalid model list."))?;
    Ok(data
        .iter()
        .filter(|entry| {
            (!honor_active || entry.get("active").and_then(Value::as_bool).unwrap_or(true))
                && entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(is_text_generation_model)
        })
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            Some(ModelRecord {
                name: id.clone(),
                id,
                description: entry
                    .get("owned_by")
                    .and_then(Value::as_str)
                    .map(|owner| format!("Provided by {owner}"))
                    .unwrap_or_default(),
                context_window: entry.get("context_window").and_then(Value::as_u64),
                supports_tools: None,
                reasoning_efforts: exposed_reasoning_efforts(entry),
                default_reasoning_effort: exposed_default_effort(entry),
            })
        })
        .collect())
}

fn fetch_anthropic_models(
    client: &Client,
    provider: ProviderId,
    api_key: &str,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let mut models = Vec::new();
    let mut after_id: Option<String> = None;
    loop {
        let mut request = client
            .get(format!("{}/models", provider.base_url()))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .query(&[("limit", "1000")]);
        if let Some(cursor) = &after_id {
            request = request.query(&[("after_id", cursor)]);
        }
        let value = get_json(request, provider)?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| DiscoveryError::invalid("Anthropic returned an invalid model list."))?;
        models.extend(data.iter().filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            Some(ModelRecord {
                name: entry
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                id,
                description: String::new(),
                context_window: entry.get("max_input_tokens").and_then(Value::as_u64),
                supports_tools: Some(true),
                reasoning_efforts: exposed_reasoning_efforts(entry),
                default_reasoning_effort: exposed_default_effort(entry),
            })
        }));
        if !value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
        after_id = value
            .get("last_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if after_id.is_none() {
            break;
        }
    }
    Ok(models)
}

fn fetch_gemini_models(
    client: &Client,
    provider: ProviderId,
    api_key: &str,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let mut models = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut request = client
            .get(format!("{}/models", provider.base_url()))
            .header("x-goog-api-key", api_key)
            .query(&[("pageSize", "1000")]);
        if let Some(token) = &page_token {
            request = request.query(&[("pageToken", token)]);
        }
        let value = get_json(request, provider)?;
        let data = value
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| DiscoveryError::invalid("Gemini returned an invalid model list."))?;
        models.extend(data.iter().filter_map(|entry| {
            let methods = entry.get("supportedGenerationMethods")?.as_array()?;
            if !methods
                .iter()
                .any(|method| method.as_str() == Some("generateContent"))
            {
                return None;
            }
            let resource_name = entry.get("name")?.as_str()?;
            let id = resource_name
                .strip_prefix("models/")
                .unwrap_or(resource_name)
                .to_string();
            Some(ModelRecord {
                name: entry
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                id,
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                context_window: entry.get("inputTokenLimit").and_then(Value::as_u64),
                supports_tools: None,
                reasoning_efforts: exposed_reasoning_efforts(entry),
                default_reasoning_effort: exposed_default_effort(entry),
            })
        }));
        page_token = value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }
    Ok(models)
}

fn fetch_openrouter_models(
    client: &Client,
    provider: ProviderId,
    api_key: &str,
) -> Result<Vec<ModelRecord>, DiscoveryError> {
    let value = get_json(
        client
            .get(format!("{}/models", provider.base_url()))
            .bearer_auth(api_key)
            .query(&[("limit", "1000"), ("output_modalities", "text")]),
        provider,
    )?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| DiscoveryError::invalid("OpenRouter returned an invalid model list."))?;
    Ok(data
        .iter()
        .filter(|entry| {
            entry
                .pointer("/architecture/output_modalities")
                .and_then(Value::as_array)
                .is_none_or(|modalities| {
                    modalities
                        .iter()
                        .any(|modality| modality.as_str() == Some("text"))
                })
        })
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let supports_tools = entry
                .get("supported_parameters")
                .and_then(Value::as_array)
                .map(|parameters| {
                    parameters.iter().any(|parameter| {
                        matches!(parameter.as_str(), Some("tools" | "tool_choice"))
                    })
                });
            Some(ModelRecord {
                name: entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                id,
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                context_window: entry.get("context_length").and_then(Value::as_u64),
                supports_tools,
                reasoning_efforts: exposed_reasoning_efforts(entry),
                default_reasoning_effort: exposed_default_effort(entry),
            })
        })
        .collect())
}

fn exposed_reasoning_efforts(entry: &Value) -> Vec<String> {
    let values = entry
        .pointer("/reasoning/supported_efforts")
        .or_else(|| entry.get("supported_reasoning_efforts"))
        .or_else(|| entry.get("reasoning_efforts"))
        .or_else(|| entry.pointer("/capabilities/reasoning_efforts"))
        .and_then(Value::as_array);
    let Some(values) = values else {
        return Vec::new();
    };
    let allowed = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    let mut output = values
        .iter()
        .filter_map(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| allowed.contains(&value.as_str()))
        .collect::<Vec<_>>();
    output.dedup();
    output
}

fn exposed_default_effort(entry: &Value) -> Option<String> {
    entry
        .pointer("/reasoning/default_effort")
        .or_else(|| entry.get("default_reasoning_effort"))
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
}

fn get_json(
    request: reqwest::blocking::RequestBuilder,
    provider: ProviderId,
) -> Result<Value, DiscoveryError> {
    let response = request.send().map_err(|error| {
        DiscoveryError::network(format!("Could not reach {}: {error}", provider.name()))
    })?;
    let status = response.status();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(DiscoveryError::authentication(provider));
    }
    let body = response.text().map_err(|error| {
        DiscoveryError::invalid(format!(
            "Could not read {}'s response: {error}",
            provider.name()
        ))
    })?;
    let value = serde_json::from_str::<Value>(&body).map_err(|_| {
        DiscoveryError::invalid(format!(
            "{} returned unreadable model data.",
            provider.name()
        ))
    })?;
    if !status.is_success() {
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Model discovery failed");
        return Err(DiscoveryError::provider(format!(
            "{}: {}",
            provider.name(),
            truncate_error(message)
        )));
    }
    Ok(value)
}

fn truncate_error(message: &str) -> String {
    message.chars().take(240).collect()
}

fn is_text_generation_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ![
        "embedding",
        "moderation",
        "whisper",
        "transcri",
        "tts",
        "speech",
        "audio",
        "realtime",
        "image",
        "dall-e",
        "sora",
        "veo",
        "guard",
        "rerank",
    ]
    .iter()
    .any(|excluded| id.contains(excluded))
}

fn deduplicate(models: &mut Vec<ModelRecord>) {
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.id.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn catalog_contains_the_five_interim_providers() {
        assert_eq!(ProviderId::ALL.len(), 5);
        assert_eq!(ProviderId::OpenAi.base_url(), "https://api.openai.com/v1");
        assert_eq!(
            ProviderId::Gemini.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn openai_parser_excludes_non_conversational_models() {
        let value = serde_json::json!({
            "data": [
                {"id": "gpt-5", "owned_by": "openai"},
                {"id": "text-embedding-3-large", "owned_by": "openai"},
                {"id": "gpt-image-1", "owned_by": "openai"}
            ]
        });
        let models = parse_openai_list(&value, false).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5");
    }

    #[test]
    fn reasoning_efforts_are_read_from_provider_model_metadata() {
        let value = serde_json::json!({
            "data": [{
                "id": "reasoning-model",
                "owned_by": "provider",
                "reasoning": {
                    "supported_efforts": ["max", "high", "low", "unsupported"],
                    "default_effort": "high"
                }
            }]
        });
        let models = parse_openai_list(&value, false).unwrap();
        assert_eq!(models[0].reasoning_efforts, ["max", "high", "low"]);
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn provider_key_and_recent_model_survive_reload() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("indus-provider-test-{unique}"));
        let path = root.join("providers.json");
        let model = ModelRecord {
            id: "model-one".into(),
            name: "Model One".into(),
            description: String::new(),
            context_window: Some(200_000),
            supports_tools: Some(true),
            reasoning_efforts: vec!["low".to_string(), "high".to_string()],
            default_reasoning_effort: Some("high".to_string()),
        };
        let mut store = ProviderStore::at(path.clone());
        store
            .accept_models(
                ProviderId::Anthropic,
                vec![model.clone()],
                Some("secret".into()),
            )
            .unwrap();
        store.select_model(ProviderId::Anthropic, &model).unwrap();

        let reloaded = ProviderStore::at(path.clone());
        assert_eq!(reloaded.api_key(ProviderId::Anthropic), Some("secret"));
        assert_eq!(
            reloaded
                .active_selection()
                .map(|model| model.model_id.as_str()),
            Some("model-one")
        );
        assert_eq!(reloaded.cached_models(ProviderId::Anthropic), &[model]);
        assert_eq!(reloaded.active_context_window(), Some(200_000));
        assert_eq!(reloaded.active_reasoning_effort(), Some("high"));

        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
