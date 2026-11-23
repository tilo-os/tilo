//! The hosted judge: Anthropic's Messages API, configured as a provider
//! profile like every other model in Tilo, its key in the host's secret
//! store and never in a record, the environment or a log.

use std::time::Duration;

use agent_provider_config::transform::{self, Ask, Output, Sampling};
use agent_provider_config::{ModelDescriptor, ProviderProfile, ProviderProtocol, wire};
use tilo_secrets::SecretStore;
use zeroize::Zeroizing;

use crate::judge::{Case, INSTRUCTIONS, Judge, JudgeError, Verdict};

/// The app id the host's secret store keeps the moderation key under.
pub const SECRET_APP: &str = "so.tilo.moderation";
/// The purpose of the key item in that store; the item is the profile id.
pub const KEY_PURPOSE: &str = "model-api-key";

const MAX_OUTPUT_TOKENS: u32 = 1024;
/// The longest answer a judgement is read from.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// How much of its effort the model spends on the structured answer.
const EFFORT: &str = "low";
const TIMEOUT: Duration = Duration::from_secs(60);

/// The profile a workspace host uses when its operator configures none: the
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
    /// Where the judge asks and how: its profile, with the one model it asks.
    profile: ProviderProfile,
    model: String,
    key: Zeroizing<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for AnthropicJudge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicJudge")
            .field("endpoint", &self.profile.endpoint)
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
            profile: profile.clone(),
            model,
            key,
            http,
        })
    }

    /// The request that judges `case`.
    ///
    /// # Errors
    /// The ask is no request the profile's wire can carry.
    pub fn body(&self, case: &Case) -> Result<serde_json::Value, JudgeError> {
        let input = case.input();
        transform::profile_body(
            &self.profile,
            &self.ask(INSTRUCTIONS, &input, &Verdict::schema()),
        )
        .map_err(JudgeError::Provider)
    }

    /// The single-shot ask of `input`, held to the JSON `schema`: the
    /// shapes of a single-shot call are
    /// [`transform`](agent_provider_config::transform)'s.
    fn ask<'a>(
        &'a self,
        instructions: &'a str,
        input: &'a str,
        schema: &'a serde_json::Value,
    ) -> Ask<'a> {
        Ask {
            model: &self.model,
            instructions,
            input,
            max_tokens: Some(MAX_OUTPUT_TOKENS),
            sampling: Sampling::ProviderDefault,
            output: Output::Schema {
                schema,
                effort: EFFORT,
            },
        }
    }

    async fn judged(&self, case: &Case) -> Result<Verdict, JudgeError> {
        let input = case.input();
        let schema = Verdict::schema();
        self.send(&self.ask(INSTRUCTIONS, &input, &schema))
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
        self.send(&self.ask("", prompt, schema)).await
    }

    async fn send(&self, ask: &Ask<'_>) -> Result<String, JudgeError> {
        let body = transform::profile_body(&self.profile, ask).map_err(JudgeError::Provider)?;
        let mut request = self.http.post(wire::url(&self.profile)).json(&body);
        for header in transform::headers(ProviderProtocol::Anthropic, Some(self.key.as_str()))
            .map_err(JudgeError::Provider)?
        {
            request = request.header(header.name.as_ref(), header.value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|error| JudgeError::Provider(error.without_url().to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|_| JudgeError::Provider(format!("an unreadable answer ({status})")))?;
        if !status.is_success() {
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/message")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "no message".into());
            return Err(JudgeError::Provider(format!("{status}: {message}")));
        }
        let text = transform::profile_answer(&self.profile, ask, &text, MAX_ANSWER_BYTES)
            .map_err(JudgeError::Answer)?;
        if text.trim().is_empty() {
            return Err(JudgeError::Answer("no text in the answer".into()));
        }
        Ok(text)
    }
}

impl Judge for AnthropicJudge {
    fn model(&self) -> &str {
        &self.model
    }

    fn judge(
        &self,
        case: &Case,
    ) -> impl std::future::Future<Output = Result<Verdict, JudgeError>> + Send {
        self.judged(case)
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

/// The profile id a workspace host keeps its moderation key under.
pub const HOST_PROFILE: &str = "moderation";

/// The judge of a workspace host whose operator mounts the key as a file from
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
