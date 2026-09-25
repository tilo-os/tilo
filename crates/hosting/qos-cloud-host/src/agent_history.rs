//! An agent's own durable history, handed to it with every task.
//!
//! A managed agent process is disposable: a host restart, a crash or an idle
//! exit ends it, and whatever it remembered in memory goes with it. Its
//! history is not disposable. The conversation is already durable in the
//! private agent ledger (each `Admitted` request and its `TaskFinished`
//! reply, the same facts the owner's agent reads project). What it *did* is
//! recorded here: at step begin the host notes the exact operation of each
//! step — app, record, resource, action and a bounded summary of its
//! arguments — in an append-only per-agent sidecar next to the Store
//! (`<store>/agent-history/<agent>.jsonl`); the step's outcome is read back
//! from the ledger's committed step result.
//!
//! At dispatch [`for_dispatch`] joins the two into a compact JSON history of
//! that agent's earlier tasks — newest in full, older ones summarised, under a
//! hard byte cap — which rides in the task envelope over the agent's own
//! authenticated enrolled channel. It is private to that agent, carries no
//! authority (every step is still authorized by the broker and the grants)
//! and is display input for the model only.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use qos_agent_control_protocol::{AgentRequest, decode_request};
use qos_peer_state::owner_commit::{AgentRuntimeFact, PrivateAgentState, TaskOutcomeV2};
use serde::{Deserialize, Serialize};

/// Hard cap of the JSON history in one envelope.
pub(crate) const MAX_HISTORY_BYTES: usize = runtime_collab::MAX_ENROLLED_HISTORY_BYTES;
/// Earlier tasks shown with their steps and full request/reply.
const FULL_TASKS: usize = 6;
/// Most earlier tasks considered at all.
const MOST_TASKS: usize = 40;
/// Most step notes one agent's sidecar keeps (older ones are compacted away).
const MOST_NOTES: usize = 400;
/// Longest string argument kept whole in a note; longer ones keep a head.
const ARG_TEXT_WHOLE: usize = 160;
const ARG_TEXT_HEAD: usize = 120;
/// Most bytes of one note's argument summary.
const MOST_ARGS_BYTES: usize = 480;

/// One step as noted at begin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StepNote {
    pub invocation: String,
    pub step: u32,
    pub app: String,
    pub record: String,
    pub resource: String,
    pub action: String,
    pub args: String,
    pub at_ms: u64,
}

/// One step in the history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryStep {
    pub app: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub record: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resource: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    pub outcome: String,
}

/// One earlier task in the history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryTask {
    pub at_ms: u64,
    pub request: String,
    #[serde(default)]
    pub reply: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<HistoryStep>,
    /// Steps left out of an older task.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_steps: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// One workspace member, as the agent may name or @mention them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Person {
    pub name: String,
    pub role: String,
    /// The person this agent works for (who sent the task).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requester: bool,
}

/// Most people listed.
const MOST_PEOPLE: usize = 24;

/// The history as the agent receives it (`v` 1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct History {
    pub v: u8,
    /// Earlier tasks, oldest first.
    pub tasks: Vec<HistoryTask>,
    /// Older tasks left out entirely.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// Current workspace members (names as the roster shows them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<Person>,
}

fn head(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_owned();
    }
    let mut end = most;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A bounded, readable summary of a step's JSON argument object: short
/// strings whole, long ones as a head plus their length.
pub(crate) fn summarize_args(argument: Option<&str>) -> String {
    let Some(raw) = argument else {
        return String::new();
    };
    let json = raw.find('{').map_or(raw, |at| &raw[at..]);
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json) else {
        return head(raw, MOST_ARGS_BYTES);
    };
    let summary: serde_json::Map<String, serde_json::Value> = map
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::String(text) if text.chars().count() > ARG_TEXT_WHOLE => {
                    serde_json::Value::String(format!(
                        "{} ({} characters)",
                        head(&text, ARG_TEXT_HEAD),
                        text.chars().count()
                    ))
                }
                other => other,
            };
            (key, value)
        })
        .collect();
    head(
        &serde_json::Value::Object(summary).to_string(),
        MOST_ARGS_BYTES,
    )
}

fn sidecar(root: &Path, agent: &str) -> Option<PathBuf> {
    // Agent ids are enrolled labels; refuse anything that could leave the dir.
    let safe = !agent.is_empty()
        && agent.len() <= 128
        && agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        && !agent.starts_with('.');
    safe.then(|| root.join("agent-history").join(format!("{agent}.jsonl")))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Read one agent's notes (a missing or damaged line is skipped).
pub(crate) fn read_notes(root: &Path, agent: &str) -> Vec<StepNote> {
    let Some(path) = sidecar(root, agent) else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Note one step at begin. Idempotent per `(invocation, step)`: a retried
/// step is noted once. Best effort: a failed write loses only display
/// history, never an effect.
pub(crate) fn note(root: &Path, agent: &str, mut note: StepNote) {
    let Some(path) = sidecar(root, agent) else {
        return;
    };
    let mut notes = read_notes(root, agent);
    if notes
        .iter()
        .any(|seen| seen.invocation == note.invocation && seen.step == note.step)
    {
        return;
    }
    if note.at_ms == 0 {
        note.at_ms = now_ms();
    }
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if notes.len() >= MOST_NOTES {
            // Compact: keep the newest half, rewritten atomically.
            notes.push(note);
            let keep = notes.split_off(notes.len() - MOST_NOTES / 2);
            let text: String = keep
                .iter()
                .filter_map(|n| serde_json::to_string(n).ok())
                .map(|line| line + "\n")
                .collect();
            let temporary = path.with_extension("jsonl.tmp");
            std::fs::write(&temporary, text)?;
            return std::fs::rename(temporary, &path);
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let line = serde_json::to_string(&note).map_err(std::io::Error::other)?;
        // No fsync on the owner thread (a step's latency matters more than
        // the last note surviving a power cut; a process restart keeps it).
        file.write_all(format!("{line}\n").as_bytes())
    })();
    if let Err(error) = result {
        tracing::debug!(%error, "agent history note not written");
    }
}

/// Note the step an enrolled agent begins (the `AgentSteps::begin` hook).
pub(crate) fn note_step(
    root: &Path,
    agent: &str,
    invocation: uuid::Uuid,
    step: u32,
    intent: &ui::host_worker::managed_sender::ChildIntent,
) {
    note(
        root,
        agent,
        StepNote {
            invocation: invocation.to_string(),
            step,
            app: intent.binding.app_id.clone(),
            record: intent.binding.record_id.clone(),
            resource: intent.resource.clone(),
            action: intent.action.clone(),
            args: summarize_args(intent.argument.as_deref()),
            at_ms: 0,
        },
    );
}

/// A step's outcome from its committed result (`InvocationUpdate` JSON).
fn step_outcome(result_json: Option<&str>) -> String {
    let Some(value) =
        result_json.and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    else {
        return "pending".into();
    };
    match value.get("state").and_then(serde_json::Value::as_str) {
        Some("succeeded") => "succeeded".into(),
        Some("failed") => format!(
            "failed: {}",
            head(
                value
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                160
            )
        ),
        Some("cancelled") => "cancelled".into(),
        _ => "pending".into(),
    }
}

/// One earlier task as read from the ledger.
#[derive(Clone, Debug, Default)]
pub(crate) struct TaskRow {
    pub invocation: String,
    pub at_ms: u64,
    pub request: String,
    pub reply: Option<String>,
    pub outcome: Option<String>,
    /// Committed step results by step index.
    pub results: BTreeMap<u32, String>,
}

/// Join ledger rows (oldest first) and step notes into the bounded history
/// JSON, or `None` when there is nothing earlier.
pub(crate) fn build(
    rows: &[TaskRow],
    notes: &[StepNote],
    people: &[Person],
    cap: usize,
) -> Option<String> {
    if rows.is_empty() && people.is_empty() {
        return None;
    }
    let people: Vec<Person> = people.iter().take(MOST_PEOPLE).cloned().collect();
    let skipped = rows.len().saturating_sub(MOST_TASKS);
    let rows = &rows[skipped..];
    // Hard cap, keeping the newest detail longest: first summarise older
    // full tasks (down to one), then drop the oldest tasks.
    for drop in 0..=rows.len() {
        for full in (1..=FULL_TASKS).rev() {
            let mut history = compose(&rows[drop..], notes, full, skipped + drop);
            history.people.clone_from(&people);
            let json = serde_json::to_string(&history).ok()?;
            if json.len() <= cap {
                return Some(json);
            }
        }
    }
    None
}

/// The history of `rows` with the last `full` tasks in full detail.
fn compose(rows: &[TaskRow], notes: &[StepNote], full: usize, omitted: usize) -> History {
    let full_from = rows.len().saturating_sub(full);
    let tasks = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let full = index >= full_from;
            let mut steps: Vec<&StepNote> = notes
                .iter()
                .filter(|note| note.invocation == row.invocation)
                .collect();
            steps.sort_by_key(|note| note.step);
            let mut outcome_steps: Vec<HistoryStep> = steps
                .iter()
                .map(|note| HistoryStep {
                    app: note.app.clone(),
                    record: note.record.clone(),
                    action: note.action.clone(),
                    resource: if full {
                        note.resource.clone()
                    } else {
                        String::new()
                    },
                    args: if full {
                        note.args.clone()
                    } else {
                        String::new()
                    },
                    outcome: step_outcome(row.results.get(&note.step).map(String::as_str)),
                })
                .collect();
            let limit = if full { 16 } else { 3 };
            let more_steps = outcome_steps.len().saturating_sub(limit);
            outcome_steps.truncate(limit);
            let most = if full { 1_200 } else { 160 };
            HistoryTask {
                at_ms: row.at_ms,
                request: head(row.request.trim(), most),
                reply: head(row.reply.as_deref().unwrap_or("").trim(), most),
                outcome: row.outcome.clone().unwrap_or_else(|| "unfinished".into()),
                steps: outcome_steps,
                more_steps,
            }
        })
        .collect();
    History {
        v: 1,
        tasks,
        omitted,
        people: Vec::new(),
    }
}

/// The history of `agent`'s earlier tasks for the task `current` being
/// dispatched, from the ledger and the step notes. Best effort: `None` when
/// the ledger is unavailable or there is nothing earlier.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "called only by the Linux agent runtime")
)]
pub(crate) fn for_dispatch(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    agent: &str,
    current: uuid::Uuid,
    requester: [u8; 32],
) -> Option<String> {
    let store = owner.store().ok()?;
    let people = people(&store, requester);
    let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
        return None;
    };
    let current = current.to_string();
    let mut receipts: Vec<_> = ledger.facts.values().collect();
    receipts.sort_by_key(|receipt| receipt.generation);
    let mut rows: Vec<TaskRow> = Vec::new();
    for receipt in &receipts {
        let AgentRuntimeFact::Admitted {
            ids,
            intent,
            timestamp_ms,
            ..
        } = &receipt.change.fact
        else {
            continue;
        };
        let invocation = &receipt.change.invocation;
        if ids.agent_id != agent || *invocation == current {
            continue;
        }
        let request = match decode_request(intent.request.as_bytes()) {
            Ok(AgentRequest::Spawn { task, .. }) => task,
            Ok(AgentRequest::Message { text, .. }) => text,
            _ => continue,
        };
        let mut row = TaskRow {
            invocation: invocation.clone(),
            at_ms: *timestamp_ms,
            request,
            ..TaskRow::default()
        };
        if let Some(finished) = ledger.facts.get(&format!("{invocation}:task_finished"))
            && let AgentRuntimeFact::TaskFinished { outcome, text, .. } = &finished.change.fact
        {
            row.reply = Some(text.clone());
            row.outcome = Some(
                match outcome {
                    TaskOutcomeV2::Succeeded => "succeeded",
                    TaskOutcomeV2::Failed => "failed",
                    TaskOutcomeV2::Cancelled => "cancelled",
                }
                .into(),
            );
        } else if ledger
            .facts
            .contains_key(&format!("{invocation}:task_unknown"))
        {
            row.outcome = Some("unknown".into());
        }
        for step in 0..qos_peer_state::owner_commit::MAX_AGENT_TASK_STEPS {
            let Some(committed) = ledger
                .facts
                .get(&format!("{invocation}:task_step_committed:{step}"))
            else {
                continue;
            };
            if let AgentRuntimeFact::TaskStepCommitted { result_json, .. } = &committed.change.fact
            {
                row.results.insert(step, result_json.clone());
            }
        }
        rows.push(row);
    }
    drop(store);
    build(
        &rows,
        &read_notes(owner.root(), agent),
        &people,
        MAX_HISTORY_BYTES,
    )
}

/// Current members by their roster names, the requester first.
fn people(store: &crate::Store, requester: [u8; 32]) -> Vec<Person> {
    let Ok(now) = store.policy_now_ns() else {
        return Vec::new();
    };
    let Ok(peers) = store.peer_shared() else {
        return Vec::new();
    };
    let peer = &peers.1;
    let owner = peer.owner_user_public_key();
    let Ok(members) = peer.governed_members(crate::now_time(now)) else {
        return Vec::new();
    };
    let mut people: Vec<Person> = members
        .into_iter()
        .filter(|member| !member.removed)
        .filter_map(|member| {
            let name = member.display_name.as_deref()?.trim();
            (!name.is_empty()).then(|| Person {
                name: head(name, 80),
                role: if member.user_public_key == owner {
                    "owner".into()
                } else {
                    format!("{:?}", member.role).to_lowercase()
                },
                requester: member.user_public_key == requester,
            })
        })
        .collect();
    people.sort_by_key(|person| !person.requester);
    people
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(invocation: &str, request: &str, reply: &str) -> TaskRow {
        TaskRow {
            invocation: invocation.into(),
            at_ms: 1,
            request: request.into(),
            reply: Some(reply.into()),
            outcome: Some("succeeded".into()),
            results: BTreeMap::new(),
        }
    }

    fn step(invocation: &str, index: u32, action: &str, args: &str) -> StepNote {
        StepNote {
            invocation: invocation.into(),
            step: index,
            app: "so.tilo.word-processor".into(),
            record: "tab:1".into(),
            resource: "word-processor.document".into(),
            action: action.into(),
            args: args.into(),
            at_ms: 5,
        }
    }

    #[test]
    fn notes_survive_a_restart_and_are_written_once_per_step() {
        let dir = tempfile::tempdir().unwrap();
        let a = step("inv-1", 0, "type-text", r#"{"text":"Thunder"}"#);
        note(dir.path(), "agent-1", a.clone());
        note(dir.path(), "agent-1", a.clone());
        note(
            dir.path(),
            "agent-1",
            step("inv-1", 1, "new-paragraph", "{}"),
        );
        // A fresh read (as after a host restart) finds both, once each.
        let read = read_notes(dir.path(), "agent-1");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0], a);
        assert!(read_notes(dir.path(), "agent-2").is_empty());
        // Nothing can be written outside the history directory.
        note(dir.path(), "../escape", a);
        assert!(!dir.path().join("escape.jsonl").exists());
    }

    #[test]
    fn the_sidecar_is_compacted_to_a_bound() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..(MOST_NOTES as u32 + 10) {
            note(
                dir.path(),
                "agent-1",
                step(&format!("inv-{index}"), 0, "type-text", "{}"),
            );
        }
        let read = read_notes(dir.path(), "agent-1");
        assert!(read.len() <= MOST_NOTES, "{}", read.len());
        assert_eq!(
            read.last().unwrap().invocation,
            format!("inv-{}", MOST_NOTES + 9)
        );
    }

    #[test]
    fn long_text_arguments_keep_a_head_and_their_length() {
        let long = "Thunderstorms ".repeat(40);
        let summary = summarize_args(Some(&format!(
            "{}{}",
            "tilo.arguments/1:",
            serde_json::json!({"text": long, "count": 3})
        )));
        assert!(summary.contains("Thunderstorms"), "{summary}");
        assert!(summary.contains("(560 characters)"), "{summary}");
        assert!(summary.contains("\"count\":3"), "{summary}");
        assert!(summary.len() <= MOST_ARGS_BYTES + 4);
    }

    #[test]
    fn the_history_joins_requests_steps_outcomes_and_replies() {
        let mut first = row("inv-1", "Add a paragraph about thunderstorms", "Added it.");
        first.results.insert(
            0,
            r#"{"state":"succeeded","invocation":"x","output":{}}"#.into(),
        );
        first
            .results
            .insert(1, r#"{"state":"failed","invocation":"x","error":{"code":"c","message":"no caret","retryable":false}}"#.into());
        let second = row("inv-2", "Post hello in Chat", "Posted.");
        let notes = vec![
            step("inv-1", 1, "type-text", r#"{"text":"Thunder"}"#),
            step("inv-1", 0, "new-paragraph", "{}"),
            StepNote {
                app: "so.tilo.chat".into(),
                record: String::new(),
                resource: "chat.channel:general".into(),
                action: "post".into(),
                ..step("inv-2", 0, "post", r#"{"value":"hello"}"#)
            },
        ];
        let json = build(&[first, second], &notes, &[], MAX_HISTORY_BYTES).unwrap();
        let history: History = serde_json::from_str(&json).unwrap();
        assert_eq!(history.tasks.len(), 2);
        let steps = &history.tasks[0].steps;
        assert_eq!(steps[0].action, "new-paragraph");
        assert_eq!(steps[0].outcome, "succeeded");
        assert_eq!(steps[1].args, r#"{"text":"Thunder"}"#);
        assert_eq!(steps[1].outcome, "failed: no caret");
        assert_eq!(history.tasks[1].steps[0].app, "so.tilo.chat");
        assert_eq!(history.tasks[1].reply, "Posted.");
        assert_eq!(build(&[], &notes, &[], MAX_HISTORY_BYTES), None);
        // People alone still make a history (the first task has no past).
        let people = vec![Person {
            name: "Ada".into(),
            role: "owner".into(),
            requester: true,
        }];
        let json = build(&[], &notes, &people, MAX_HISTORY_BYTES).unwrap();
        assert!(
            json.contains("\"people\":[{\"name\":\"Ada\",\"role\":\"owner\",\"requester\":true}]"),
            "{json}"
        );
    }

    /// Older tasks are summarised and the whole stays under the cap,
    /// dropping the oldest first: memory grows slowly and never unbounded.
    #[test]
    fn the_history_is_tiered_and_hard_capped() {
        let rows: Vec<TaskRow> = (0..60)
            .map(|index| {
                row(
                    &format!("inv-{index}"),
                    &format!("request {index} {}", "x".repeat(900)),
                    &format!("reply {index} {}", "y".repeat(900)),
                )
            })
            .collect();
        let notes: Vec<StepNote> = (0..60)
            .flat_map(|index| {
                (0..20)
                    .map(move |s| step(&format!("inv-{index}"), s, "type-text", r#"{"text":"t"}"#))
            })
            .collect();
        let json = build(&rows, &notes, &[], MAX_HISTORY_BYTES).unwrap();
        assert!(json.len() <= MAX_HISTORY_BYTES, "{}", json.len());
        let history: History = serde_json::from_str(&json).unwrap();
        let last = history.tasks.last().unwrap();
        assert!(last.request.starts_with("request 59"));
        assert!(last.request.len() > 500, "the newest task is kept in full");
        assert_eq!(last.more_steps, 4);
        let oldest = &history.tasks[0];
        assert!(oldest.request.len() < 200, "older tasks are summarised");
        assert!(oldest.steps.iter().all(|step| step.args.is_empty()));
        assert!(history.omitted >= 20);
    }
}
