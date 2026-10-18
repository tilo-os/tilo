//! Extract old confirmation bodies before replacing their replicated history.
use super::*;
use serde_json::Value as Json;
use tilo_desktop_model::{
    WorkspaceTaskConfirmation as Confirmation, WorkspaceTaskConfirmationState as State,
};

fn public(full: &Confirmation) -> Confirmation {
    let mut result = full.clone();
    result.draft = tilo_actions::Draft::default();
    match &mut result.state {
        State::Confirmed { edits, .. } => edits.clear(),
        State::Declined { reason, .. } => *reason = None,
        State::Waiting => {}
    }
    result
}

fn confirmation(control: &Json) -> Result<Option<Confirmation>, String> {
    control
        .get("confirmation")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
        .transpose()
}

/// Called for every historical frontier; never infer a current confirmation
/// from an archived answer, even if the request id happens to match.
pub(super) fn collect(
    room: &AutoCommit,
    recovered: &mut BTreeMap<String, NativeTaskPrivateText>,
) -> Result<(), String> {
    for (id, bytes) in blobs(room, TASK_CONTROL_KEY)? {
        let control: Json = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(full) = confirmation(&control)? {
            let archive = &mut recovered.entry(id).or_default().historical_confirmations;
            if !archive.contains(&full) {
                archive.push(full);
            }
        }
    }
    for (id, bytes) in blobs(room, tilo_desktop_model::projection::ROOM_TASKS_KEY)? {
        let Ok(task) = serde_json::from_slice::<Json>(&bytes) else {
            continue;
        };
        if let Some(events) = task.get("audit").and_then(Json::as_array) {
            for event in events {
                let mut scrubbed = event.clone();
                if neutralize_event(&mut scrubbed)? {
                    let archive = &mut recovered
                        .entry(id.clone())
                        .or_default()
                        .historical_confirmation_audit;
                    if !archive.contains(event) {
                        archive.push(event.clone());
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn scrub_controls(
    room: &mut AutoCommit,
    recovered: &mut BTreeMap<String, NativeTaskPrivateText>,
) -> Result<(), String> {
    for (id, bytes) in blobs(room, TASK_CONTROL_KEY)? {
        let mut control: Json = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(full) = confirmation(&control)? {
            let scrubbed = public(&full);
            // An already-sanitized placeholder is not recovered private custody.
            if full != scrubbed {
                recovered.entry(id.clone()).or_default().confirmation = Some(full);
            }
            control["confirmation"] = serde_json::to_value(scrubbed).map_err(|e| e.to_string())?;
            put_blob(room, TASK_CONTROL_KEY, &id, &bytes, &control)?;
        }
    }
    Ok(())
}

pub(super) fn validate_controls(room: &AutoCommit) -> Result<(), String> {
    for (_, bytes) in blobs(room, TASK_CONTROL_KEY)? {
        let control: Json = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(full) = confirmation(&control)?
            && full != public(&full)
        {
            return Err("baseline contains private task confirmation bodies".into());
        }
    }
    Ok(())
}

pub(super) fn neutralize_task(task: &mut Json) -> Result<(), String> {
    if let Some(events) = task.get_mut("audit") {
        for event in events.as_array_mut().ok_or("task audit is not a list")? {
            neutralize_event(event)?;
        }
    }
    Ok(())
}

fn neutralize_event(event: &mut Json) -> Result<bool, String> {
    if !event.is_object() {
        return Err("task audit event is not an object".into());
    }
    let affected = if let Some(what) = event.get_mut("what") {
        neutralize_what(what)?
    } else {
        false
    };
    if affected && let Some(fields) = event.as_object_mut() {
        fields.remove("note");
    }
    Ok(affected)
}

fn neutralize_what(what: &mut Json) -> Result<bool, String> {
    if !what.is_object() {
        return Err("task audit action is not an object".into());
    }
    let mut affected = false;
    if matches!(
        what.pointer("/action/action").and_then(Json::as_str),
        Some("ask-confirmation" | "confirm-action" | "decline-action")
    ) {
        // Old versions wrote the private draft label into args.action. Drop all
        // arguments rather than guessing which future argument might be prose.
        what["args"] = serde_json::json!({});
        affected = true;
    }
    for field in ["members", "distinct"] {
        if let Some(members) = what.get_mut(field) {
            for member in members
                .as_array_mut()
                .ok_or("task audit members are not a list")?
            {
                affected |= neutralize_what(member)?;
            }
        }
    }
    if let Some(previous) = what.pointer_mut("/of/what") {
        affected |= neutralize_what(previous)?;
    }
    Ok(affected)
}
