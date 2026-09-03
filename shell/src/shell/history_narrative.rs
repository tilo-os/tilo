//! Private, local-model history prose.
//!
//! The shell discovers an already-installed Ollama model and runs it off the
//! UI thread. It never downloads a model and never sends workspace evidence
//! to a remote endpoint. Generated prose is still admitted only through the
//! UI crate's citation validator.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ui::{GroundedNarrative, NarrativeRequest};

#[derive(Default)]
pub struct LocalHistoryNarrator {
    pending: Arc<std::sync::Mutex<Option<(String, GroundedNarrative)>>>,
    running: Arc<AtomicBool>,
    last_key: Option<String>,
}

impl LocalHistoryNarrator {
    /// Start one bounded background generation when the evidence changes.
    pub fn request(&mut self, request: &NarrativeRequest) {
        let prompt = prompt_for(request);
        let key = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        if self.last_key.as_deref() == Some(&key) || self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.last_key = Some(key.clone());
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
        let expected = blake3::hash(prompt_for(request).as_bytes())
            .to_hex()
            .to_string();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .and_then(|(key, narrative)| (key == expected).then_some(narrative))
    }
}

fn installed_model() -> Option<String> {
    let output = Command::new("ollama")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    let models = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect::<Vec<_>>();
    if let Ok(requested) = std::env::var("QOS_HISTORY_LLM_MODEL")
        && !requested.trim().is_empty()
    {
        return models.into_iter().find(|model| model == requested.trim());
    }
    models.into_iter().next()
}

fn generate(prompt: &str) -> Option<GroundedNarrative> {
    let model = installed_model()?;
    let mut child = Command::new("ollama")
        .args(["run", &model, prompt])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let raw = String::from_utf8(output.stdout).ok()?;
    parse_response(&raw, &model)
}

fn prompt_for(request: &NarrativeRequest) -> String {
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
        "Rewrite the workspace activity evidence as one clear, factual catch-up sentence. \
Never invent intent, status, outcomes, names, or facts. Use 45 words or fewer. \
Return JSON only: {{\"text\":\"...\",\"source_ids\":[\"32 lowercase hex chars\"]}}. \
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
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let offset = index.saturating_mul(2);
        *slot = u8::from_str_radix(value.get(offset..offset.saturating_add(2))?, 16).ok()?;
    }
    Some(ui::widget::HistoryEntryId::from_bytes(bytes))
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
    fn strict_json_response_preserves_grounding_ids() {
        let raw = r#"{"text":"Ada refined the brief.","source_ids":["01010101010101010101010101010101"]}"#;
        let parsed = parse_response(raw, "local-test").expect("valid response");
        assert_eq!(parsed.text, "Ada refined the brief.");
        assert_eq!(parsed.source_ids, vec![ui::widget::HistoryEntryId([1; 16])]);
        assert_eq!(parsed.generator, "ollama:local-test");
        assert!(parse_response(&format!("Here: {raw}"), "local-test").is_none());
    }

    #[test]
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
}
