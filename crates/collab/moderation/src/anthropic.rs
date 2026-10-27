//! The hosted judge: Anthropic's Messages API, configured as a provider
//! profile like every other model in Tilo, its key in the host's secret
//! store and never in a record, the environment or a log.

use std::time::Duration;

use agent_provider_config::{ModelDescriptor, ProviderProfile, ProviderProtocol};
use tilo_secrets::SecretStore;
use zeroize::Zeroizing;

use crate::judge::{Case, INSTRUCTIONS, Judge, JudgeError, Verdict};

/// The app id the host's secret store keeps the moderation key under.
pub const SECRET_APP: &str = "so.tilo.moderation";
/// The purpose of the key item in that store; the item is the profile id.
pub const KEY_PURPOSE: &str = "model-api-key";

const API_VERSION: &str = "2023-06-01";
const MAX_OUTPUT_TOKENS: u32 = 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

/// The profile a room host uses when its operator configures none: the
/// hosted Anthropic endpoint and [`crate::DEFAULT_MODEL`], or `model`.
#[must_use]
pub fn profile(id: &str, model: Option<&str>) -> ProviderProfile {
    let model = model.unwrap_or(crate::DEFAULT_MODEL);
    let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    id.clone_into(&mut profile.id);
    "Moderation".clone_into(&mut profile.label);
    profile.credential = None;
    profile.models = vec![ModelDescriptor::new(model)];
    profile.default_model = Some(model.to_owned());
    profile
}

/// Asks an Anthropic model.
pub struct AnthropicJudge {
    endpoint: String,
    model: String,
    key: Zeroizing<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for AnthropicJudge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicJudge")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl AnthropicJudge {
    /// The judge `profile` describes, its key read from `store`.
    pub fn from_profile(
        profile: &ProviderProfile,
        store: &SecretStore,
    ) -> Result<Self, JudgeError> {
        if profile.protocol != ProviderProtocol::Anthropic || !profile.enabled {
            return Err(JudgeError::Unconfigured(
                "the moderation profile is not an enabled Anthropic profile".into(),
            ));
        }
        let model = profile
            .default_model
            .clone()
            .ok_or_else(|| JudgeError::Unconfigured("the profile names no model".into()))?;
        let key = store
            .item(KEY_PURPOSE, &profile.id)
            .get_blocking()
            .map_err(|error| JudgeError::Unconfigured(format!("the key store failed: {error}")))?
            .ok_or_else(|| JudgeError::Unconfigured("no API key is stored".into()))?;
        let key = Zeroizing::new(
            String::from_utf8(key.expose().to_vec())
                .map_err(|_| JudgeError::Unconfigured("the stored key is not text".into()))?
                .trim()
                .to_owned(),
        );
        let http = tilo_net::http::client()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| JudgeError::Unconfigured("no HTTP client".into()))?;
        Ok(Self {
            endpoint: profile.endpoint.trim_end_matches('/').to_owned(),
            model,
            key,
            http,
        })
    }

    #[must_use]
    pub fn body(&self, case: &Case) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": INSTRUCTIONS,
            "output_config": {
                "effort": "low",
                "format": {"type": "json_schema", "schema": Verdict::schema()}
            },
            "messages": [{"role": "user", "content": case.input()}]
        })
    }

    async fn ask(&self, case: &Case) -> Result<Verdict, JudgeError> {
        self.send(&self.body(case))
            .await
            .and_then(|text| Verdict::parse(&text))
    }

    /// Ask the model `prompt` alone, its answer held to the JSON `schema`:
    /// the host lends the same model to another contract than moderation,
    /// such as the privacy review of its reporters' reports.
    ///
    /// # Errors
    /// The provider failed, refused, or cut the answer off.
    pub async fn complete(
        &self,
        prompt: &str,
        schema: &serde_json::Value,
    ) -> Result<String, JudgeError> {
        self.send(&serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "output_config": {
                "effort": "low",
                "format": {"type": "json_schema", "schema": schema}
            },
            "messages": [{"role": "user", "content": prompt}]
        }))
        .await
    }

    async fn send(&self, body: &serde_json::Value) -> Result<String, JudgeError> {
        let response = self
            .http
            .post(format!("{}/v1/messages", self.endpoint))
            .header("x-api-key", self.key.as_str())
            .header("anthropic-version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|error| JudgeError::Provider(error.without_url().to_string()))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| JudgeError::Provider(format!("an unreadable answer ({status})")))?;
        if !status.is_success() {
            let message = body
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no message");
            return Err(JudgeError::Provider(format!("{status}: {message}")));
        }
        answer_text(&body)
    }
}

/// The text of a Messages answer, refusing a refusal or a cut-off answer.
///
/// # Errors
/// The answer was refused, truncated or carried no text.
pub fn answer_text(body: &serde_json::Value) -> Result<String, JudgeError> {
    match body["stop_reason"].as_str() {
        Some("refusal") => return Err(JudgeError::Answer("the model declined".into())),
        Some("max_tokens") => return Err(JudgeError::Answer("the answer was cut off".into())),
        _ => {}
    }
    let text: String = body["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    if text.trim().is_empty() {
        return Err(JudgeError::Answer("no text in the answer".into()));
    }
    Ok(text)
}

impl Judge for AnthropicJudge {
    fn model(&self) -> &str {
        &self.model
    }

    fn judge(
        &self,
        case: &Case,
    ) -> impl std::future::Future<Output = Result<Verdict, JudgeError>> + Send {
        self.ask(case)
    }
}

/// Stores the moderation key for `profile` in `store`, as an operator's
/// mounted secret is imported at start.
///
/// # Errors
/// The store refused it.
pub fn store_key(store: &SecretStore, profile: &ProviderProfile, key: &[u8]) -> Result<(), String> {
    store
        .item(KEY_PURPOSE, &profile.id)
        .put_blocking(key)
        .map_err(|error| error.to_string())
}

/// The profile id a room host keeps its moderation key under.
pub const HOST_PROFILE: &str = "moderation";

/// The judge of a room host whose operator mounts the key as a file from
/// their encrypted secrets: the key is kept in the host's secret store, as
/// every provider key is, and the judge reads it from there. `model` is the
/// operator's choice, [`crate::DEFAULT_MODEL`] when `None`.
///
/// # Errors
/// The file is unreadable or empty, or the store refused the key. The key
/// itself never appears in an error.
pub fn import_mounted_key(
    store: &SecretStore,
    key_file: &std::path::Path,
    model: Option<&str>,
) -> Result<AnthropicJudge, JudgeError> {
    let key = Zeroizing::new(std::fs::read(key_file).map_err(|error| {
        JudgeError::Unconfigured(format!("read {}: {}", key_file.display(), error.kind()))
    })?);
    let trimmed = key.trim_ascii();
    if trimmed.is_empty() {
        return Err(JudgeError::Unconfigured(format!(
            "{} is empty",
            key_file.display()
        )));
    }
    let profile = profile(HOST_PROFILE, model);
    store_key(store, &profile, trimmed).map_err(JudgeError::Unconfigured)?;
    AnthropicJudge::from_profile(&profile, store)
}

#[cfg(test)]
mod tests;
