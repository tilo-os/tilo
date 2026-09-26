//! Private, local-model history prose.
//!
//! The shell discovers an already-installed Ollama model and runs it off the
//! UI thread. It never downloads a model and never sends workspace evidence
//! to a remote endpoint. Generated prose is still admitted only through the
//! UI crate's citation validator.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

    fn request_prompt(&mut self, _request: &NarrativeRequest, prompt: String) {
        if !self.enabled {
            return;
        }
        #[cfg(test)]
        crate::shell::test_support::ambient_start("history model worker");
        let key = blake3::hash(prompt.as_bytes()).to_hex().to_string();
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
                *pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, narrative));
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

    fn take_for_prompt(&mut self, prompt: &str) -> Option<GroundedNarrative> {
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

fn generate(prompt: &str) -> Option<GroundedNarrative> {
    let model = installed_model()?;
    let mut command = Command::new("ollama");
    command.args(["run", &model, prompt]);
    let output =
        bounded_command::run_prepared(command, None, NARRATIVE_LIMIT, NARRATIVE_TIMEOUT).ok()?;
    output.status.success().then_some(())?;
    let raw = String::from_utf8(output.stdout).ok()?;
    parse_response(&raw, &model)
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
        "Write a short present-tense status describing what the local participant appears to be working toward right now. Use a natural verb phrase, no name or pronoun, and 12 words or fewer. Infer only from the supplied recent actions; never invent an outcome or motivation.",
    )
}

fn evidence_prompt(request: &NarrativeRequest, instruction: &str) -> String {
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
    format!(
        "{instruction} Return JSON only: {{\"text\":\"...\",\"source_ids\":[\"32 lowercase hex chars\"]}}. \
Include only source_ids that directly support the sentence, and at least one.\nEvidence: {}",
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
    let source_ids = value
        .get("source_ids")?
        .as_array()?
        .iter()
        .map(|id| parse_hex_id(id.as_str()?))
        .collect::<Option<Vec<_>>>()?;
    Some(GroundedNarrative {
        text,
        source_ids,
        generator: format!("ollama:{model}"),
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
                    source_ids: old.source_ids,
                    generator: "test".into(),
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
