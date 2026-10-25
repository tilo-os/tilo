//! The opt-in JSON-lines logs a run is traced and replayed from:
//! `TILO_AGENT_PLAN_LOG` (every plan) and `TILO_AGENT_ACTION_LOG` (every
//! invoked action). Neither holds a picture's bytes.

use tilo_actions::ActionOutput;
use tilo_actions::door::ActionAddress;
use tilo_browser_peer::DocumentRoute;

use super::running::{FollowUp, produced, step_brief};
use crate::planner::{Obligations, Step};

/// Appends one JSON line per plan to `TILO_AGENT_PLAN_LOG`, when set: the
/// task, what was planned (its text, or the rest of it, with the follow-up
/// turn it is), and the steps or why there are none, so a task that ended
/// early can be traced to its plan. With them, the obligations the plan
/// declared (each step's in `steps_for`) and the checklist it was shown,
/// each item open, done or set aside.
pub(super) fn record_plan(
    agent: &str,
    task: &str,
    text: &str,
    (continues, turn): (bool, Option<FollowUp>),
    by_model: bool,
    steps: &Result<Vec<Step>, String>,
    (declared, shown): (&Obligations, Option<serde_json::Value>),
) {
    let Some(path) = std::env::var_os("TILO_AGENT_PLAN_LOG") else {
        return;
    };
    let serves: Option<Vec<Option<usize>>> = steps
        .as_ref()
        .ok()
        .filter(|steps| steps.iter().any(|step| step.serves.is_some()))
        .map(|steps| steps.iter().map(|step| step.serves).collect());
    let line = serde_json::json!({
        "at_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "task": task,
        "continues": continues,
        "turn": turn.map(|turn| format!("{turn:?}")),
        "by_model": by_model,
        "text": text.chars().take(4_000).collect::<String>(),
        "steps": steps.as_ref().ok().map(|steps| steps.iter().map(step_brief).collect::<Vec<_>>()),
        "error": steps.as_ref().err(),
        "obligations": (!declared.items.is_empty()).then_some(&declared.items),
        "steps_for": serves,
        "set_aside": (!declared.set_aside.is_empty()).then_some(&declared.set_aside),
        "checklist": shown,
    });
    append_line(&path, &line, "a plan");
}

/// Appends to `TILO_AGENT_PLAN_LOG`, when set, the end of a task that kept
/// a checklist: its outcome and each obligation as it ended.
pub(super) fn record_ended(
    agent: &str,
    task: &str,
    outcome: &Result<String, String>,
    checklist: &serde_json::Value,
) {
    let Some(path) = std::env::var_os("TILO_AGENT_PLAN_LOG") else {
        return;
    };
    let line = serde_json::json!({
        "at_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "task": task,
        "ended": match outcome {
            Ok(done) => done,
            Err(error) => error,
        },
        "ok": outcome.is_ok(),
        "checklist": checklist,
    });
    append_line(&path, &line, "a task's end");
}

/// One write per line, as the usage log does: agents append at once.
fn append_line(path: &std::ffi::OsStr, line: &serde_json::Value, what: &str) {
    use std::io::Write as _;
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record {what}");
    }
}

/// Appends one JSON line per invoked action to `TILO_AGENT_ACTION_LOG`, when
/// set: who did what to which record, with its arguments, when, and how it
/// went. A run can be replayed from it, every record rebuilt step by step.
pub(super) fn record_action(
    agent: &str,
    route: &DocumentRoute,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    started_ms: u64,
    outcome: &Result<Option<ActionOutput>, String>,
) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("TILO_AGENT_ACTION_LOG") else {
        return;
    };
    let line = serde_json::json!({
        "started_unix_ms": started_ms,
        "ended_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "app": route.app_id,
        "record": route.record,
        "resource": address.resource,
        "action": address.action,
        "arguments": arguments,
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err(),
        "produced": outcome.as_ref().ok().and_then(Option::as_ref).map(produced).unwrap_or_default(),
        // The resources it answered with: a replay maps the ids it made (a
        // new sheet's) onto the ones the replay makes.
        "resources": outcome
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|output| serde_json::to_value(&output.resources).unwrap_or_default()),
        "value": outcome.as_ref().ok().and_then(Option::as_ref).and_then(|output| output.value.clone()),
    });
    // One write per line, as the usage log does: agents append at once.
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record an action");
    }
}
