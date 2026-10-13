//! Optional history prose through the user's configured Ollama runner.
//!
//! Model selection uses `TILO_HISTORY_LLM_MODEL`; Ollama owns endpoint configuration
//! (including `OLLAMA_HOST`). No credentials or parallel gateway are introduced.
//! Citation checks establish source membership, not truth or entailment.

use std::process::Command;
use std::sync::Arc;

use std::time::{Duration, Instant};

use ui::{GroundedNarrative, NarrativeRequest};

type CommandFactory = dyn Fn(&[&str]) -> Command + Send + Sync;

#[derive(Default)]
struct RunnerState {
    generation: u64,
    running: bool,
    pending: Option<(String, GroundedNarrative)>,
    feedback: Option<(String, &'static str)>,
}

pub struct LocalHistoryNarrator {
    enabled: bool,
    state: Arc<std::sync::Mutex<RunnerState>>,
    cancel: Option<Arc<bounded_command::Cancellation>>,
    command: Arc<CommandFactory>,
    worker: Option<std::thread::JoinHandle<()>>,
    #[cfg(test)]
    injected: bool,
    last_key: Option<String>,
    completed_key: Option<String>,
    last_attempt: Option<Instant>,
}

impl Default for LocalHistoryNarrator {
    fn default() -> Self {
        Self {
            enabled: true,
            state: Arc::default(),
            cancel: None,
            worker: None,
            #[cfg(test)]
            injected: false,
            command: Arc::new(|args| {
                let mut command = Command::new("ollama");
                command.args(args);
                command
            }),
            last_key: None,
            completed_key: None,
            last_attempt: None,
        }
    }
}

impl Drop for LocalHistoryNarrator {
    fn drop(&mut self) {
        self.invalidate();
    }
}

const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Automatic selection avoids evicting the foreground app's working set.
/// An explicit model override remains the user's informed choice.
const MAX_AUTOMATIC_MODEL_BYTES: u64 = 5_000_000_000;
const MODEL_LIST_LIMIT: usize = 1024 * 1024;
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(5);
const NARRATIVE_LIMIT: usize = 1024 * 1024;
const NARRATIVE_TIMEOUT: Duration = Duration::from_secs(20);

impl LocalHistoryNarrator {
    pub(crate) fn disabled() -> Self {
        let mut narrator = Self::default();
        narrator.enabled = false;
        narrator
    }

    pub fn request(&mut self, request: &NarrativeRequest) {
        self.request_prompt(request, prompt_for(request));
    }

    pub fn request_status(&mut self, request: &NarrativeRequest) {
        self.request_prompt(request, status_prompt_for(request));
    }

    fn request_prompt(&mut self, request: &NarrativeRequest, prompt: String) {
        if !self.enabled {
            return;
        }
        let key = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        // Revoke on every transition, including A → B → A. Prompt equality is
        // not authority to publish from a previous lifetime of that prompt.
        if self.last_key.as_deref() != Some(&key) {
            self.invalidate();
        }
        if self.completed_key.as_deref() == Some(&key)
            || (self.last_key.as_deref() == Some(&key)
                && self
                    .last_attempt
                    .is_some_and(|attempt| attempt.elapsed() < FAILED_RETRY_DELAY))
        {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.running {
            return;
        }
        let Ok(cancel) = bounded_command::Cancellation::new() else {
            state.feedback = Some((key, "activity-summary-discovery-failed"));
            return;
        };
        #[cfg(test)]
        if !self.injected {
            crate::shell::test_support::ambient_start("history model worker");
        }
        let cancel = Arc::new(cancel);
        self.cancel = Some(Arc::clone(&cancel));
        self.last_key = Some(key.clone());
        self.last_attempt = Some(Instant::now());
        state.running = true;
        state.feedback = Some((key.clone(), "activity-summary-pending"));
        let generation = state.generation;
        drop(state);
        let shared = Arc::clone(&self.state);
        let command = Arc::clone(&self.command);
        let requested_model = std::env::var("TILO_HISTORY_LLM_MODEL").ok();
        let request = request.clone();
        self.worker = Some(std::thread::spawn(move || {
            let current = || {
                !cancel.is_cancelled()
                    && shared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .generation
                        == generation
            };
            let outcome = generate(
                &prompt,
                requested_model.as_deref(),
                &cancel,
                command.as_ref(),
                &current,
            )
            .and_then(|mut narrative| {
                narrative.request = Some(Box::new(request.clone()));
                ui::validate_narrative(&request, narrative).ok_or("activity-summary-failed")
            });
            let mut state = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation != generation || cancel.is_cancelled() {
                return;
            }
            state.running = false;
            match outcome {
                Ok(narrative) => {
                    state.feedback = Some((key.clone(), "activity-summary-ready"));
                    state.pending = Some((key, narrative));
                }
                Err(feedback) => state.feedback = Some((key, feedback)),
            }
        }));
    }

    /// Revocation is monotonic even if the evidence later returns to the same key.
    pub fn invalidate(&mut self) {
        // Revoke dispatch authority before advancing publication authority.
        // Cancellation serializes with spawn, so there is no gap in which a
        // revoked generation can send its prompt to the configured endpoint.
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(generation) = state.generation.checked_add(1) else {
            // Fail closed rather than ever reusing a publication authority.
            std::process::abort();
        };
        state.generation = generation;
        state.pending = None;
        state.feedback = None;
        state.running = false;
        drop(state);
        self.completed_key = None;
        self.last_key = None;
        self.last_attempt = None;
    }

    pub fn feedback_for(&self, request: &NarrativeRequest) -> &'static str {
        let expected = blake3::hash(prompt_for(request).as_bytes())
            .to_hex()
            .to_string();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .feedback
            .as_ref()
            .filter(|(key, _)| *key == expected)
            .map_or("activity-summary-pending", |(_, state)| *state)
    }

    pub fn take_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&prompt_for(request))
    }

    pub fn take_status_for(&mut self, request: &NarrativeRequest) -> Option<GroundedNarrative> {
        self.take_for_prompt(&status_prompt_for(request))
    }

    fn take_for_prompt(&mut self, prompt: &str) -> Option<GroundedNarrative> {
        let expected = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        let narrative = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .take()
            .and_then(|(key, narrative)| (key == expected).then_some(narrative));
        if narrative.is_some() {
            self.completed_key = Some(expected);
        }
        narrative
    }
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

fn generate(
    prompt: &str,
    requested: Option<&str>,
    cancel: &bounded_command::Cancellation,
    command: &CommandFactory,
    current: &impl Fn() -> bool,
) -> Result<GroundedNarrative, &'static str> {
    if !current() {
        return Err("activity-summary-pending");
    }
    let output = bounded_command::run_prepared_cancellable(
        command(&["list"]),
        None,
        MODEL_LIST_LIMIT,
        MODEL_LIST_TIMEOUT,
        cancel,
    )
    .map_err(|_| "activity-summary-discovery-failed")?;
    if !output.status.success() {
        return Err("activity-summary-discovery-failed");
    }
    let list = String::from_utf8(output.stdout).map_err(|_| "activity-summary-discovery-failed")?;
    let model = select_installed_model(&list, requested).ok_or("activity-summary-unavailable")?;
    if !current() {
        return Err("activity-summary-pending");
    }
    let output = bounded_command::run_prepared_cancellable(
        command(&["run", &model, prompt]),
        None,
        NARRATIVE_LIMIT,
        NARRATIVE_TIMEOUT,
        cancel,
    )
    .map_err(|_| "activity-summary-failed")?;
    if !current() || !output.status.success() {
        return Err("activity-summary-failed");
    }
    let raw = String::from_utf8(output.stdout).map_err(|_| "activity-summary-failed")?;
    parse_response(&raw, &model).ok_or("activity-summary-failed")
}

#[cfg(test)]
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
    fn injected_subprocess_generations_revoke_even_when_the_key_returns() {
        let (entered, events) = std::sync::mpsc::channel();
        let (release, gates) = std::sync::mpsc::channel();
        let gates = std::sync::Mutex::new(gates);
        let model = std::env::var("TILO_HISTORY_LLM_MODEL")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "fixture:test".into());
        let inventory = format!("NAME ID SIZE MODIFIED\n{} id 1 MB now", model.trim());
        let mut narrator = LocalHistoryNarrator::default();
        narrator.injected = true;
        narrator.command = Arc::new(move |args| {
            // Pause before run dispatch. No model service or global
            // environment mutation is involved in this regression.
            if args[0] == "run" {
                entered.send(()).unwrap();
                gates.lock().unwrap().recv().unwrap();
            }
            let text = if args[0] == "list" {
                inventory.as_str()
            } else {
                r#"{"text":"Recorded edits","source_ids":["01010101010101010101010101010101"]}"#
            };
            let mut command = Command::new("sh");
            command.args(["-c", "printf '%s' \"$1\"", "fixture", text]);
            command
        });
        let a = request(1, "Recorded edits");
        let b = request(2, "Other edits");
        narrator.request(&a);
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        let first = narrator.worker.take().unwrap();
        let first_generation = narrator.state.lock().unwrap().generation;
        narrator.request(&b);
        // Release A before allowing B through the same deterministic gate.
        release.send(()).unwrap();
        first.join().unwrap();
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = narrator.worker.take().unwrap();
        narrator.request(&a);
        release.send(()).unwrap();
        second.join().unwrap();
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(narrator.state.lock().unwrap().generation > first_generation);
        assert_eq!(narrator.feedback_for(&a), "activity-summary-pending");
        assert!(narrator.take_for(&a).is_none());
        release.send(()).unwrap();
        narrator.worker.take().unwrap().join().unwrap();
        assert_eq!(narrator.feedback_for(&a), "activity-summary-ready");
        assert!(narrator.take_for(&a).is_some());
        narrator.invalidate();
        assert!(narrator.take_for(&a).is_none());
        assert_eq!(narrator.feedback_for(&a), "activity-summary-pending");
    }

    #[test]
    fn changed_evidence_cancels_an_inflight_discovery_tree_without_waiting_for_timeout() {
        use std::io::Read as _;
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        assert!(
            Command::new("mkfifo")
                .arg(&ready)
                .status()
                .unwrap()
                .success()
        );
        let (sender, received) = std::sync::mpsc::channel();
        let reader_path = ready.clone();
        let reader = std::thread::spawn(move || {
            let mut pipe = std::fs::File::open(reader_path).unwrap();
            let mut byte = [0];
            pipe.read_exact(&mut byte).unwrap();
            sender.send(()).unwrap();
        });
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut narrator = LocalHistoryNarrator::default();
        narrator.injected = true;
        narrator.command = Arc::new(move |args| {
            assert_eq!(args, ["list"]);
            let mut command = Command::new("sh");
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                command
                    .args(["-c", "sleep 30 & printf x > \"$1\"; wait", "fixture"])
                    .arg(&ready);
            } else {
                command.args(["-c", "printf 'NAME ID SIZE MODIFIED\\n'"]);
            }
            command
        });
        let a = request(1, "Recorded edits");
        narrator.request(&a);
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        let old_worker = narrator.worker.take().unwrap();
        let began = Instant::now();
        let b = request(2, "Other edits");
        narrator.request(&b);
        old_worker.join().unwrap();
        assert!(began.elapsed() < Duration::from_secs(1));
        narrator.worker.take().unwrap().join().unwrap();
        assert_eq!(narrator.feedback_for(&b), "activity-summary-unavailable");
        assert_eq!(narrator.feedback_for(&a), "activity-summary-pending");
        assert!(narrator.take_for(&a).is_none());
        reader.join().unwrap();
    }

    #[test]
    fn injected_discovery_distinguishes_failure_from_no_installed_model() {
        let cancel = bounded_command::Cancellation::new().unwrap();
        for (script, expected) in [
            ("exit 7", "activity-summary-discovery-failed"),
            (
                "printf 'NAME ID SIZE MODIFIED\\n'",
                "activity-summary-unavailable",
            ),
        ] {
            let factory: Arc<CommandFactory> = Arc::new(move |args| {
                assert_eq!(args, ["list"], "discovery must not dispatch generation");
                let mut command = Command::new("sh");
                command.args(["-c", script]);
                command
            });
            assert_eq!(
                generate("evidence", None, &cancel, factory.as_ref(), &|| true).unwrap_err(),
                expected
            );
        }
        cancel.cancel();
        assert_eq!(
            generate(
                "evidence",
                None,
                &cancel,
                &|_| panic!("revoked worker dispatched"),
                &|| false
            )
            .unwrap_err(),
            "activity-summary-pending"
        );
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
            narrator.state.lock().unwrap().pending = Some((
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
