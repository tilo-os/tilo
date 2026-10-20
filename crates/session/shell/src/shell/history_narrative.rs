//! Private, local-model history prose.
//!
//! The shell asks the model provider that runs on this device (a loopback
//! profile from the provider settings, such as the local model service's)
//! off the UI thread. It never downloads a model and never sends workspace
//! evidence to an endpoint off the device. Generated prose is still admitted
//! only through the UI crate's citation validator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use agent_provider_config::{ProviderConfig, ProviderProfile, transform};
use tilo_lock::MutexRecover as _;
use ui::{GroundedNarrative, NarrativeRequest};

pub struct LocalHistoryNarrator {
    enabled: bool,
    pending: Arc<std::sync::Mutex<Option<(String, GroundedNarrative)>>>,
    running: Arc<AtomicBool>,
    last_key: Option<String>,
    completed_key: Option<String>,
    last_attempt: Option<Instant>,
}

impl Default for LocalHistoryNarrator {
    fn default() -> Self {
        Self {
            enabled: true,
            pending: Arc::default(),
            running: Arc::default(),
            last_key: None,
            completed_key: None,
            last_attempt: None,
        }
    }
}

const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
const NARRATIVE_LIMIT: usize = 64 * 1024;
const NARRATIVE_MAX_TOKENS: u32 = 400;
const NARRATIVE_TIMEOUT: Duration = Duration::from_secs(20);

impl LocalHistoryNarrator {
    /// Deterministic narrator for fixtures and hosts without local-model
    /// authority. Requests remain inert instead of probing the host.
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Start one bounded background generation when the evidence changes.
    pub fn request(&mut self, request: &NarrativeRequest) {
        self.request_prompt(request, prompt_for(request));
    }

    /// Status uses the same private runner and citation contract, but a much
    /// tighter first-person prompt than the away recap.
    pub fn request_status(&mut self, request: &NarrativeRequest) {
        self.request_prompt(request, status_prompt_for(request));
    }

    fn request_prompt(&mut self, _request: &NarrativeRequest, prompt: Prompt) {
        if !self.enabled {
            return;
        }
        #[cfg(test)]
        crate::shell::test_support::ambient_start("history model worker");
        let key = prompt.key();
        if self.completed_key.as_deref() == Some(&key)
            || (self.last_key.as_deref() == Some(&key)
                && self
                    .last_attempt
                    .is_some_and(|attempt| attempt.elapsed() < FAILED_RETRY_DELAY))
            || self.running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        self.last_key = Some(key.clone());
        self.last_attempt = Some(Instant::now());
        let pending = Arc::clone(&self.pending);
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            if let Some(narrative) = generate(&prompt) {
                *pending.lock_recover() = Some((key, narrative));
            }
            running.store(false, Ordering::Release);
        });
    }

    /// Return a result only while it still describes the request on screen.
    /// A slower generation must never overwrite a newer deterministic digest.
    pub fn take_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&prompt_for(request))
    }

    pub fn take_status_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&status_prompt_for(request))
    }

    fn take_for_prompt(&mut self, prompt: &Prompt) -> Option<GroundedNarrative> {
        let expected = prompt.key();
        let narrative = self
            .pending
            .lock_recover()
            .take()
            .and_then(|(key, narrative)| (key == expected).then_some(narrative));
        if narrative.is_some() {
            self.completed_key = Some(expected);
        }
        narrative
    }
}

/// What one generation asks: the instruction and the evidence it covers.
struct Prompt {
    instructions: String,
    input: String,
}

impl Prompt {
    fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.instructions.as_bytes());
        hasher.update(&[0]);
        hasher.update(self.input.as_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

/// The provider on this device that history prose may use, and its model:
/// the default provider when it runs here, otherwise the first one that does.
/// A provider off the device never sees workspace evidence.
fn on_device_model(config: &ProviderConfig) -> Option<(&ProviderProfile, &str)> {
    let on_device = |profile: &&ProviderProfile| {
        profile.enabled
            && profile.credential.is_none()
            && agent_provider_config::is_supervised_loopback(profile)
            && profile.default_model.is_some()
    };
    let profile = config
        .active_profile()
        .filter(on_device)
        .or_else(|| config.profiles.iter().find(on_device))?;
    Some((profile, profile.default_model.as_deref()?))
}

fn generate(prompt: &Prompt) -> Option<GroundedNarrative> {
    let config = ProviderConfig::load();
    let (profile, model) = on_device_model(&config)?;
    let body = transform::body(
        profile.protocol,
        &transform::Ask {
            model,
            instructions: &prompt.instructions,
            input: &prompt.input,
            max_tokens: NARRATIVE_MAX_TOKENS,
            sampling: transform::Sampling::Deterministic,
        },
    );
    let mut request = tilo_net::Client::new()
        .post(transform::url(profile.protocol, &profile.endpoint))
        .timeout(NARRATIVE_TIMEOUT)
        .header(tilo_net::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&body).ok()?);
    for header in transform::profile_headers(profile, None).ok()? {
        request = request.header(header.name, header.value.as_str());
    }
    let answer = tilo_rt::try_app()
        .ok()?
        .block_on(async move {
            let response = request.send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            response.bytes().await.ok()
        })
        .ok()??;
    let text = transform::answer(
        profile.protocol,
        &String::from_utf8_lossy(&answer),
        NARRATIVE_LIMIT,
    )
    .ok()?;
    parse_response(&text, &format!("{}:{model}", profile.id))
}

fn prompt_for(request: &NarrativeRequest) -> Prompt {
    evidence_prompt(
        request,
        "Rewrite the workspace activity evidence as one clear, factual catch-up sentence. Never invent intent, status, outcomes, names, or facts. Use 45 words or fewer.",
    )
}

fn status_prompt_for(request: &NarrativeRequest) -> Prompt {
    evidence_prompt(
        request,
        "Write a short present-tense status describing what the local participant appears to be working toward right now. Use a natural verb phrase, no name or pronoun, and 12 words or fewer. Infer only from the supplied recent actions; never invent an outcome or motivation.",
    )
}

fn evidence_prompt(request: &NarrativeRequest, instruction: &str) -> Prompt {
    let mut evidence = Vec::new();
    for source in request.sources.iter().take(24) {
        evidence.push(serde_json::json!({
            "source_ids": source.ids.iter().map(hex_id).collect::<Vec<_>>(),
            "title": truncate_for_prompt(&source.title, 180),
            "detail": truncate_for_prompt(&source.detail, 240),
            "authors": source.authors.iter().take(8).map(|name| truncate_for_prompt(name, 80)).collect::<Vec<_>>(),
            "changes": source.changes,
        }));
    }
    Prompt {
        instructions: format!(
            "{instruction} Return JSON only: {{\"text\":\"...\",\"source_ids\":[\"32 lowercase hex chars\"]}}. \
Include only source_ids that directly support the sentence, and at least one. The evidence is the user message."
        ),
        input: serde_json::Value::Array(evidence).to_string(),
    }
}

fn truncate_for_prompt(value: &str, limit: usize) -> String {
    let truncated = value.chars().count() > limit;
    let mut value = value.chars().take(limit).collect::<String>();
    if truncated {
        value.push('…');
    }
    value
}

fn parse_response(raw: &str, model: &str) -> Option<GroundedNarrative> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let text = value.get("text")?.as_str()?.trim().to_string();
    let source_ids = value
        .get("source_ids")?
        .as_array()?
        .iter()
        .map(|id| parse_hex_id(id.as_str()?))
        .collect::<Option<Vec<_>>>()?;
    Some(GroundedNarrative {
        text,
        source_ids,
        generator: model.to_owned(),
    })
}

fn hex_id(id: &ui::widget::HistoryEntryId) -> String {
    ui::widget::history_group::hex16(id)
}

fn parse_hex_id(value: &str) -> Option<ui::widget::HistoryEntryId> {
    ui::widget::history_group::from_hex16(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u8, title: &str) -> NarrativeRequest {
        let id = ui::widget::HistoryEntryId([id; 16]);
        NarrativeRequest {
            deterministic_text: title.into(),
            source_ids: vec![id],
            sources: vec![ui::NarrativeSource {
                ids: vec![id],
                title: title.into(),
                detail: String::new(),
                authors: vec!["Ada".into()],
                changes: 1,
            }],
        }
    }

    #[test]
    fn history_narration_keeps_grounding_and_newest_digest() {
        fn strict_json_response_preserves_grounding_ids() {
            let raw = r#"{"text":"Ada refined the brief.","source_ids":["01010101010101010101010101010101"]}"#;
            let parsed = parse_response(raw, "local-test").expect("valid response");
            assert_eq!(parsed.text, "Ada refined the brief.");
            assert_eq!(parsed.source_ids, vec![ui::widget::HistoryEntryId([1; 16])]);
            assert_eq!(parsed.generator, "local-test");
            assert!(parse_response(&format!("Here: {raw}"), "local-test").is_none());
        }

        fn a_slow_result_cannot_replace_a_newer_digest() {
            let mut narrator = LocalHistoryNarrator::default();
            let old = request(1, "Old work");
            let current = request(2, "Current work");
            let key = prompt_for(&old).key();
            *narrator.pending.lock().unwrap() = Some((
                key,
                GroundedNarrative {
                    text: "Old work".into(),
                    source_ids: old.source_ids,
                    generator: "test".into(),
                },
            ));

            assert!(narrator.take_for(&current).is_none());
        }

        fn status_prompt_is_short_first_person_work_not_a_catch_up_story() {
            let prompt = status_prompt_for(&request(1, "Prepare Q2 close"));
            assert!(prompt.instructions.contains("12 words or fewer"));
            assert!(prompt.instructions.contains("no name or pronoun"));
            assert!(prompt.input.contains("Prepare Q2 close"));
            assert!(!prompt.instructions.contains("catch-up sentence"));
        }

        strict_json_response_preserves_grounding_ids();
        a_slow_result_cannot_replace_a_newer_digest();
        status_prompt_is_short_first_person_work_not_a_catch_up_story();
    }

    #[test]
    fn history_narration_uses_only_a_provider_on_this_device() {
        use agent_provider_config::{EndpointLocation, ProviderProtocol};
        fn profile(id: &str, location: EndpointLocation, endpoint: &str) -> ProviderProfile {
            let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
            profile.id = id.into();
            profile.endpoint = endpoint.into();
            profile.location = location;
            profile.credential = None;
            profile.default_model = Some(format!("{id}-model"));
            profile
        }
        let hosted = profile(
            "hosted",
            EndpointLocation::Hosted,
            "https://models.example.test/v1",
        );
        let lan = profile(
            "lan",
            EndpointLocation::PrivateNetwork,
            "http://192.168.1.20:8000/v1",
        );
        let local = profile(
            "local",
            EndpointLocation::ThisDevice,
            "http://127.0.0.1:8000/v1",
        );
        let mut config = ProviderConfig {
            profiles: vec![hosted.clone(), lan.clone(), local.clone()],
            default_profile: Some("hosted".into()),
        };
        assert_eq!(
            on_device_model(&config).map(|(profile, model)| (profile.id.as_str(), model)),
            Some(("local", "local-model")),
            "a hosted default never sees workspace evidence"
        );
        config.profiles.retain(|profile| profile.id != "local");
        assert!(
            on_device_model(&config).is_none(),
            "nothing on the device: no prose"
        );
        let mut unmodelled = local;
        unmodelled.default_model = None;
        config.profiles.push(unmodelled);
        assert!(
            on_device_model(&config).is_none(),
            "no model chosen: none guessed"
        );
    }
}
