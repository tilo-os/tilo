//! Optional history prose through the user's configured Ollama runner.
//!
//! Model selection uses `TILO_HISTORY_LLM_MODEL`; Ollama owns endpoint configuration
//! (including `OLLAMA_HOST`). No credentials or parallel gateway are introduced.
//! Citation checks establish source membership, not truth or entailment.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ui::{GroundedNarrative, NarrativeRequest};

pub struct LocalHistoryNarrator {
    enabled: bool,
    pending: Arc<std::sync::Mutex<Option<(String, GroundedNarrative)>>>,
    running: Arc<AtomicBool>,
    feedback: Arc<std::sync::Mutex<Option<(String, &'static str)>>>,
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
            feedback: Arc::default(),
            last_key: None,
            completed_key: None,
            last_attempt: None,
        }
    }
}

const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Automatic history prose stays a background convenience rather than
/// evicting the foreground app's working set. `TILO_HISTORY_LLM_MODEL` remains
/// an explicit opt-in escape hatch for a larger installed model.
const MAX_AUTOMATIC_MODEL_BYTES: u64 = 5_000_000_000;
const MODEL_LIST_LIMIT: usize = 1024 * 1024;
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(5);
const NARRATIVE_LIMIT: usize = 1024 * 1024;
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

    fn request_prompt(&mut self, request: &NarrativeRequest, prompt: String) {
        if !self.enabled {
            return;
        }
        #[cfg(test)]
        crate::shell::test_support::ambient_start("history model worker");
        let key = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        if self.completed_key.as_deref() != Some(&key) {
            self.completed_key = None;
        }
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
        *self
            .feedback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((key.clone(), "activity-summary-pending"));
        let feedback = Arc::clone(&self.feedback);
        let request = request.clone();
        let pending = Arc::clone(&self.pending);
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            match generate(&prompt) {
                Ok(mut narrative) => {
                    narrative.request = Some(Box::new(request.clone()));
                    let Some(narrative) = ui::validate_narrative(&request, narrative) else {
                        *feedback
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some((key, "activity-summary-failed"));
                        running.store(false, Ordering::Release);
                        return;
                    };
                    *feedback
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some((key.clone(), "activity-summary-ready"));
                    *pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some((key, narrative));
                }
                Err(state) => {
                    *feedback
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, state));
                }
            }
            running.store(false, Ordering::Release);
        });
    }

    /// Return a result only while it still describes the request on screen.
    /// A slower generation must never overwrite a newer deterministic digest.
    pub fn invalidate(&mut self) {
        self.completed_key = None;
        self.last_key = None;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    pub fn feedback_for(&self, request: &NarrativeRequest) -> &'static str {
        let expected = blake3::hash(prompt_for(request).as_bytes())
            .to_hex()
            .to_string();
        self.feedback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(key, _)| *key == expected)
            .map_or("activity-summary-unavailable", |(_, state)| *state)
    }

    pub fn take_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&prompt_for(request))
    }

    pub fn take_status_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&status_prompt_for(request))
    }

    fn take_for_prompt(&mut self, prompt: &str) -> Option<GroundedNarrative> {
        // Scope and coverage are carried in the prompt identity, even when prose evidence matches.
        let expected = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        let narrative = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .and_then(|(key, narrative)| (key == expected).then_some(narrative));
        if narrative.is_some() {
            self.completed_key = Some(expected);
        }
        narrative
    }
}

fn installed_model() -> Option<String> {
    let mut command = Command::new("ollama");
    command.arg("list");
    let output =
        bounded_command::run_prepared(command, None, MODEL_LIST_LIMIT, MODEL_LIST_TIMEOUT).ok()?;
    output.status.success().then_some(())?;
    select_installed_model(
        &String::from_utf8(output.stdout).ok()?,
        std::env::var("TILO_HISTORY_LLM_MODEL").ok().as_deref(),
    )
}

fn select_installed_model(list: &str, requested: Option<&str>) -> Option<String> {
    let requested = requested.map(str::trim).filter(|name| !name.is_empty());
    let mut models = list
        .lines()
        .skip(1)
        .filter_map(|line| {
            let columns = line.split_whitespace().collect::<Vec<_>>();
            let name = (*columns.first()?).to_string();
            let size = columns.windows(2).find_map(|pair| {
                let [value, unit] = pair else { return None };
                let value = value.replace(',', "").parse::<f64>().ok()?;
                let multiplier = match unit.to_ascii_uppercase().as_str() {
                    "KB" => 1_000_f64,
                    "MB" => 1_000_000_f64,
                    "GB" => 1_000_000_000_f64,
                    "TB" => 1_000_000_000_000_f64,
                    _ => return None,
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "Model inventory sizes are nonnegative decimal byte estimates used only for bounded model selection."
                )]
                Some((value * multiplier) as u64)
            });
            Some((name, size))
        })
        .collect::<Vec<_>>();
    if let Some(requested) = requested {
        return models
            .into_iter()
            .find_map(|(name, _)| (name == requested).then_some(name));
    }
    models.retain(|(name, _)| {
        let name = name.to_ascii_lowercase();
        !["embed", "nomic", "minilm", "bge", "e5-"]
            .iter()
            .any(|marker| name.contains(marker))
    });
    models.retain(|(_, size)| size.is_some_and(|bytes| bytes <= MAX_AUTOMATIC_MODEL_BYTES));
    models.sort_by(|left, right| {
        left.1
            .unwrap_or(u64::MAX)
            .cmp(&right.1.unwrap_or(u64::MAX))
            .then_with(|| left.0.cmp(&right.0))
    });
    models.into_iter().next().map(|(name, _)| name)
}

fn generate(prompt: &str) -> Result<GroundedNarrative, &'static str> {
    let model = installed_model().ok_or("activity-summary-unavailable")?;
    generate_with(prompt, Some(model), |model, prompt| {
        let mut command = Command::new("ollama");
        command.args(["run", model, prompt]);
        let output =
            bounded_command::run_prepared(command, None, NARRATIVE_LIMIT, NARRATIVE_TIMEOUT)
                .ok()?;
        output.status.success().then_some(())?;
        String::from_utf8(output.stdout).ok()
    })
    .ok_or("activity-summary-failed")
}

fn generate_with(
    prompt: &str,
    model: Option<String>,
    run: impl FnOnce(&str, &str) -> Option<String>,
) -> Option<GroundedNarrative> {
    let model = model?;
    parse_response(&run(&model, prompt)?, &model)
}

fn prompt_for(request: &NarrativeRequest) -> String {
    evidence_prompt(
        request,
        "Rewrite the workspace activity evidence as one clear, factual catch-up sentence. Never invent intent, status, outcomes, names, or facts. Use 45 words or fewer.",
    )
}

fn status_prompt_for(request: &NarrativeRequest) -> String {
    evidence_prompt(
        request,
        "Describe the recorded recent actions, not inferred goals or current task status. Use a natural verb phrase, no name or pronoun, and 12 words or fewer. Never invent an outcome or motivation.",
    )
}

fn evidence_prompt(request: &NarrativeRequest, instruction: &str) -> String {
    let mut evidence = Vec::new();
    for source in request
        .sources
        .iter()
        .filter(|source| source.ids.iter().any(|id| request.source_ids.contains(id)))
        .take(24)
    {
        evidence.push(serde_json::json!({
            "source_ids": source.ids.iter().filter(|id| request.source_ids.contains(id)).take(64).map(hex_id).collect::<Vec<_>>(),
            "title": truncate_for_prompt(&source.title, 180),
            "detail": truncate_for_prompt(&source.detail, 240),
            "authors": source.authors.iter().take(8).map(|name| truncate_for_prompt(name, 80)).collect::<Vec<_>>(),
            "changes": source.changes,
        }));
    }
    format!(
        "Request identity: {:?}. Locale: {}. {instruction} Return JSON only: {{\"text\":\"...\",\"source_ids\":[\"32 lowercase hex chars\"]}}. \
Include only source_ids that directly support the sentence, and at least one, at most six unique IDs.\nEvidence: {}",
        blake3::hash(
            format!(
                "{request:?}:{:?}:{:?}",
                std::env::var("TILO_HISTORY_LLM_MODEL").ok(),
                std::env::var("OLLAMA_HOST").ok()
            )
            .as_bytes()
        )
        .to_hex()
        .to_string(),
        request.locale,
        serde_json::Value::Array(evidence)
    )
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
    let ids = value.get("source_ids")?.as_array()?;
    if ids.is_empty() || ids.len() > 6 {
        return None;
    }
    let source_ids = ids
        .iter()
        .map(|id| parse_hex_id(id.as_str()?))
        .collect::<Option<Vec<_>>>()?;
    Some(GroundedNarrative {
        text,
        source_ids,
        generator: model_source(model),
        request: None,
    })
}

fn model_source(model: &str) -> String {
    // Host configuration is user choice. Do not expose URL credentials or claim locality.
    let endpoint = std::env::var("OLLAMA_HOST").ok().and_then(|host| {
        let authority = host
            .split_once("://")
            .map_or(host.as_str(), |(_, rest)| rest)
            .split('/')
            .next()?
            .rsplit('@')
            .next()?;
        (!authority.is_empty()
            && authority
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)))
        .then(|| authority.to_string())
    });
    endpoint.map_or_else(
        || format!("ollama:{model}"),
        |host| format!("ollama:{model} ({host})"),
    )
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
            projection: String::new(),
            locale: "en-US".into(),
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
    fn injected_runner_respects_selection_locale_bounds_and_missing_model() {
        let mut evidence = request(1, "Recorded edits");
        evidence.locale = "de-DE".into();
        evidence
            .sources
            .extend((2..=25).map(|id| ui::NarrativeSource {
                ids: vec![ui::widget::HistoryEntryId([id; 16])],
                title: format!("brief-{id}"),
                detail: String::new(),
                authors: Vec::new(),
                changes: 1,
            }));
        evidence
            .source_ids
            .extend((2..=25).map(|id| ui::widget::HistoryEntryId([id; 16])));
        let prompt = prompt_for(&evidence);
        let model = select_installed_model(
            "NAME ID SIZE MODIFIED\nchosen:test a 2 GB now",
            Some("chosen:test"),
        );
        let mut response = generate_with(&prompt, model, |model, input| {
            assert_eq!(model, "chosen:test");
            assert!(input.contains("Locale: de-DE"));
            assert!(input.contains("brief-24"));
            assert!(!input.contains("brief-25"));
            Some(
                r#"{"text":"Recorded edits","source_ids":["01010101010101010101010101010101"]}"#
                    .into(),
            )
        })
        .unwrap();
        response.request = Some(Box::new(evidence.clone()));
        assert!(ui::validate_narrative(&evidence, response).is_some());
        assert!(
            generate_with(&prompt, None, |_, _| panic!("missing model must not run")).is_none()
        );
        assert!(generate_with(&prompt, Some("chosen:test".into()), |_, _| None).is_none());
    }

    #[test]
    fn history_narration_keeps_grounding_and_newest_digest() {
        fn strict_json_response_preserves_grounding_ids() {
            let raw = r#"{"text":"Ada refined the brief.","source_ids":["01010101010101010101010101010101"]}"#;
            let parsed = parse_response(raw, "local-test").expect("valid response");
            assert_eq!(parsed.text, "Ada refined the brief.");
            assert_eq!(parsed.source_ids, vec![ui::widget::HistoryEntryId([1; 16])]);
            assert_eq!(parsed.generator, "ollama:local-test");
            assert!(parse_response(&format!("Here: {raw}"), "local-test").is_none());
        }

        fn a_slow_result_cannot_replace_a_newer_digest() {
            let mut narrator = LocalHistoryNarrator::default();
            let old = request(1, "Old work");
            let current = request(2, "Current work");
            let key = blake3::hash(prompt_for(&old).as_bytes())
                .to_hex()
                .to_string();
            *narrator.pending.lock().unwrap() = Some((
                key,
                GroundedNarrative {
                    text: "Old work".into(),
                    source_ids: old.source_ids.clone(),
                    generator: "test".into(),
                    request: Some(Box::new(old)),
                },
            ));

            assert!(narrator.take_for(&current).is_none());
        }

        fn status_prompt_is_short_first_person_work_not_a_catch_up_story() {
            let prompt = status_prompt_for(&request(1, "Prepare Q2 close"));
            assert!(prompt.contains("12 words or fewer"));
            assert!(prompt.contains("no name or pronoun"));
            assert!(prompt.contains("Prepare Q2 close"));
            assert!(!prompt.contains("catch-up sentence"));
        }

        strict_json_response_preserves_grounding_ids();
        a_slow_result_cannot_replace_a_newer_digest();
        status_prompt_is_short_first_person_work_not_a_catch_up_story();
    }

    #[test]
    fn history_narration_uses_only_the_smallest_known_generative_model() {
        fn chooses_the_smallest_generative_model_not_the_first_or_an_embedder() {
            let list = "NAME ID SIZE MODIFIED\n\
                        llama3.1:8b abc 4.7 GB yesterday\n\
                        nomic-embed-text:latest def 274 MB yesterday\n\
                        qwen2.5:0.5b ghi 398 MB yesterday\n\
                        phi3:mini jkl 2.2 GB yesterday\n";
            assert_eq!(
                select_installed_model(list, None).as_deref(),
                Some("qwen2.5:0.5b")
            );
            assert_eq!(
                select_installed_model(list, Some("phi3:mini")).as_deref(),
                Some("phi3:mini")
            );
            assert_eq!(select_installed_model(list, Some("missing")), None);
        }

        fn automatic_narration_refuses_unknown_or_oversized_models() {
            let list = "NAME ID SIZE MODIFIED\n\
                        giant:70b abc 42 GB yesterday\n\
                        unknown-size def - yesterday\n";
            assert_eq!(select_installed_model(list, None), None);
            assert_eq!(
                select_installed_model(list, Some("giant:70b")).as_deref(),
                Some("giant:70b"),
                "an explicit override is informed opt-in"
            );
        }

        chooses_the_smallest_generative_model_not_the_first_or_an_embedder();
        automatic_narration_refuses_unknown_or_oversized_models();
    }
}
