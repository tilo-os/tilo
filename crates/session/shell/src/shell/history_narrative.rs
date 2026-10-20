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
use ui::floating_surface::{HistoryChangeDescription, HistoryChangeFacts};
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
/// The provider's whole answer, its envelope around the prose included: a
/// misbehaving endpoint cannot make the shell buffer more than this.
const NARRATIVE_RESPONSE_LIMIT: usize = 1024 * 1024;

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
    let (text, generator) = ask(prompt, NARRATIVE_MAX_TOKENS)?;
    parse_response(&text, &generator)
}

/// One answer from the model on this device, and which provider and model
/// gave it; `None` without one or when it fails.
fn ask(prompt: &Prompt, max_tokens: u32) -> Option<(String, String)> {
    let config = ProviderConfig::load();
    let (profile, model) = on_device_model(&config)?;
    let body = transform::body(
        profile.protocol,
        &transform::Ask {
            model,
            instructions: &prompt.instructions,
            input: &prompt.input,
            max_tokens,
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
            response.bytes_limited(NARRATIVE_RESPONSE_LIMIT).await.ok()
        })
        .ok()??;
    let text = transform::answer(
        profile.protocol,
        &String::from_utf8_lossy(&answer),
        NARRATIVE_LIMIT,
    )
    .ok()?;
    Some((text, format!("{}:{model}", profile.id)))
}

/// Descriptions ready to send, each with the request it answers.
type Answers = Arc<std::sync::Mutex<Vec<(u64, Vec<HistoryChangeDescription>)>>>;

/// Descriptions of apps' changes, for their History panels: asked by an
/// app over its shell-ui connection, answered off the UI thread by the model
/// on this device, one request at a time. Apps hold no model; the text they
/// get back they keep with the changes, for everyone in the workspace.
pub struct HistoryDescriber {
    enabled: bool,
    queue: std::collections::VecDeque<(u64, Vec<HistoryChangeFacts>)>,
    answers: Answers,
    running: Arc<AtomicBool>,
    /// Wakes the loop that sends answers, when one is ready.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for HistoryDescriber {
    fn default() -> Self {
        Self {
            enabled: true,
            queue: std::collections::VecDeque::new(),
            answers: Arc::default(),
            running: Arc::default(),
            wake: None,
        }
    }
}

/// Most requests waiting for the model; an app asks again when its
/// History is next open.
const MAX_QUEUED_DESCRIPTIONS: usize = 16;
const DESCRIPTION_MAX_TOKENS: u32 = 800;

impl HistoryDescriber {
    /// For fixtures and hosts without local-model authority: every request
    /// is answered with nothing.
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Wake the loop with `wake` when an answer is ready, so an idle desktop
    /// sends it at once.
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    /// Queue `changes`, asked by the app on `connection`.
    pub fn request(&mut self, connection: u64, changes: Vec<HistoryChangeFacts>) {
        if changes.is_empty() {
            return;
        }
        if !self.enabled || self.queue.len() >= MAX_QUEUED_DESCRIPTIONS {
            self.answers.lock_recover().push((connection, Vec::new()));
            return;
        }
        self.queue.push_back((connection, changes));
    }

    /// An app went away: nothing it asked is answered.
    pub fn forget(&mut self, connection: u64) {
        self.queue.retain(|(asker, _)| *asker != connection);
    }

    /// The answers ready for their apps; starts the next request.
    pub fn take_answers(&mut self) -> Vec<(u64, Vec<HistoryChangeDescription>)> {
        let ready = std::mem::take(&mut *self.answers.lock_recover());
        if !self.queue.is_empty() && !self.running.swap(true, Ordering::AcqRel) {
            #[cfg(test)]
            crate::shell::test_support::ambient_start("history describer worker");
            if let Some((connection, changes)) = self.queue.pop_front() {
                let answers = Arc::clone(&self.answers);
                let running = Arc::clone(&self.running);
                let wake = self.wake.clone();
                std::thread::spawn(move || {
                    let described = describe(&changes);
                    answers.lock_recover().push((connection, described));
                    running.store(false, Ordering::Release);
                    if let Some(wake) = wake {
                        wake();
                    }
                });
            } else {
                self.running.store(false, Ordering::Release);
            }
        }
        ready
    }
}

fn describe(changes: &[HistoryChangeFacts]) -> Vec<HistoryChangeDescription> {
    ask(&description_prompt(changes), DESCRIPTION_MAX_TOKENS)
        .map(|(text, generator)| parse_descriptions(&text, &generator, changes))
        .unwrap_or_default()
}

fn description_prompt(changes: &[HistoryChangeFacts]) -> Prompt {
    let evidence: Vec<serde_json::Value> = changes
        .iter()
        .map(|change| {
            serde_json::json!({
                "change": change.change,
                "summary": truncate_for_prompt(&change.summary, 180),
                "purpose": change.action.as_deref().map(|text| truncate_for_prompt(text, 120)),
                "item": change.resource.as_deref().map(|text| truncate_for_prompt(text, 120)),
                "wrote": truncate_for_prompt(&change.wrote, 240),
            })
        })
        .collect();
    Prompt {
        instructions: "For each change in the user message, write one short past-tense sentence of at most 12 words saying what it did to the document, from its facts only: its summary, its stated purpose, the item it touched and what it wrote. Never name a person, never guess intent or outcome, and quote at most a few words of what it wrote. Return JSON only: {\"descriptions\":[{\"change\":\"<the change's id>\",\"text\":\"...\"}]}. Leave out a change whose facts say too little.".to_owned(),
        input: serde_json::Value::Array(evidence).to_string(),
    }
}

/// The model's descriptions of changes it was asked about, each one line
/// History accepts; anything else is dropped.
fn parse_descriptions(
    raw: &str,
    generator: &str,
    asked: &[HistoryChangeFacts],
) -> Vec<HistoryChangeDescription> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return Vec::new();
    };
    let Some(items) = value
        .get("descriptions")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut described: Vec<HistoryChangeDescription> = Vec::new();
    for item in items {
        let (Some(change), Some(text)) = (
            item.get("change").and_then(serde_json::Value::as_str),
            item.get("text").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let text = text.trim();
        if asked.iter().any(|fact| fact.change == change)
            && !described.iter().any(|done| done.change == change)
            && ui::history_source::descriptions::is_acceptable(text)
        {
            described.push(HistoryChangeDescription {
                change: change.to_owned(),
                text: text.to_owned(),
                generator: generator.to_owned(),
            });
        }
    }
    described
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
    fn descriptions_are_kept_only_for_changes_asked_about_and_one_line_each() {
        let fact = |change: &str| HistoryChangeFacts {
            change: change.into(),
            summary: "Edited the body".into(),
            action: None,
            resource: None,
            wrote: "Costs rose in March".into(),
        };
        let asked = [fact("aa"), fact("bb")];
        let raw = r#"{"descriptions":[
            {"change":"aa","text":"Noted that costs rose in March"},
            {"change":"aa","text":"A second answer for the same change"},
            {"change":"zz","text":"A change nobody asked about"},
            {"change":"bb","text":"two\nlines"}
        ]}"#;
        let described = parse_descriptions(raw, "local:m", &asked);
        assert_eq!(
            described,
            vec![HistoryChangeDescription {
                change: "aa".into(),
                text: "Noted that costs rose in March".into(),
                generator: "local:m".into(),
            }]
        );
        assert!(parse_descriptions("Sure! Here you go", "m", &asked).is_empty());
        let prompt = description_prompt(&asked);
        assert!(prompt.input.contains("Costs rose in March"));
        assert!(prompt.instructions.contains("Never name a person"));
    }

    #[test]
    fn a_describer_without_a_model_answers_with_nothing_at_once() {
        let mut describer = HistoryDescriber::disabled();
        describer.request(
            7,
            vec![HistoryChangeFacts {
                change: "aa".into(),
                summary: "Edited".into(),
                action: None,
                resource: None,
                wrote: String::new(),
            }],
        );
        assert_eq!(describer.take_answers(), vec![(7, Vec::new())]);
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
            profiles: vec![hosted, lan, local.clone()],
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
