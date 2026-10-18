//! Recover automatic prose privately, then replace its public audit notes.
use crate::workspace_task::NativeTaskPrivateText;
use automerge::AutoCommit;
use serde_json::Value;
use std::collections::BTreeMap;

fn automatic(task: &Value) -> bool {
    matches!(
        task.get("route").and_then(Value::as_str),
        Some("model" | "rules_only")
    )
}

/// Historical events lack enough evidence to reconstruct authenticated transcript
/// messages. Keep the exact event, including the original body, as private archive
/// data instead of fabricating invocation, source-device or acceptance identities.
/// Only locally available history is recoverable; already-retired changes cannot
/// be recovered from a replacement checkpoint.
pub(super) fn historical_reports(
    room: &AutoCommit,
) -> Result<BTreeMap<String, NativeTaskPrivateText>, String> {
    let mut recovered = BTreeMap::new();
    collect_reports(room, &mut recovered)?;
    super::confirmations::collect(room, &mut recovered)?;
    let mut history = room.clone();
    let hashes: Vec<_> = history
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    for hash in hashes {
        let past = history.fork_at(&[hash]).map_err(|e| e.to_string())?;
        collect_reports(&past, &mut recovered)?;
        super::confirmations::collect(&past, &mut recovered)?;
    }
    Ok(recovered)
}

fn collect_reports(
    room: &AutoCommit,
    recovered: &mut BTreeMap<String, NativeTaskPrivateText>,
) -> Result<(), String> {
    for (id, bytes) in super::blobs(room, tilo_desktop_model::projection::ROOM_TASKS_KEY)? {
        let Ok(task) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if !automatic(&task) {
            continue;
        }
        for event in audit(&task)? {
            if event.get("note").is_none_or(Value::is_null) {
                continue;
            }
            event
                .get("note")
                .and_then(Value::as_str)
                .ok_or("task audit note is not text")?;
            let reports = &mut recovered.entry(id.clone()).or_default().historical_reports;
            if !reports.contains(event) {
                reports.push(event.clone());
            }
        }
    }
    for (id, bytes) in super::blobs(room, crate::workspace_task::TASK_CONTROL_KEY)? {
        if manual_control(room, &id)? {
            continue;
        }
        let control: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        for presentation in presentations(&control)?
            .into_iter()
            .flat_map(|map| map.values())
        {
            let exact: collab_types::task_presentation::TaskPresentation =
                serde_json::from_value(presentation.clone()).map_err(|e| e.to_string())?;
            let event = serde_json::json!({
                "note": exact.text,
                "presentation": presentation,
            });
            let reports = &mut recovered.entry(id.clone()).or_default().historical_reports;
            if !reports.contains(&event) {
                reports.push(event);
            }
        }
    }
    Ok(())
}

fn presentations(control: &Value) -> Result<Option<&serde_json::Map<String, Value>>, String> {
    match control.get("presentations") {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(_) => Err("task presentations are not a map".into()),
    }
}

fn manual_control(room: &AutoCommit, id: &str) -> Result<bool, String> {
    let tasks = super::blobs(room, tilo_desktop_model::projection::ROOM_TASKS_KEY)?;
    Ok(tasks
        .get(id)
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .is_some_and(|task| task.get("route").and_then(Value::as_str) == Some("manual")))
}

/// Scrub only the prose leaves. Run identity, sequence, receipt time and finish
/// remain public, along with execution, confirmation and future control fields.
pub(super) fn neutralize_presentations(control: &mut Value) -> Result<(), String> {
    presentations(control)?;
    if let Some(Value::Object(map)) = control.get_mut("presentations") {
        for value in map.values_mut() {
            let _: collab_types::task_presentation::TaskPresentation =
                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            for field in ["text", "reasoning", "detail"] {
                value[field] = "".into();
            }
        }
    }
    Ok(())
}

pub(super) fn scrub_control_presentations(room: &mut AutoCommit) -> Result<(), String> {
    for (id, bytes) in super::blobs(room, crate::workspace_task::TASK_CONTROL_KEY)? {
        if manual_control(room, &id)? {
            continue;
        }
        let mut control: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        neutralize_presentations(&mut control)?;
        super::put_blob(
            room,
            crate::workspace_task::TASK_CONTROL_KEY,
            &id,
            &bytes,
            &control,
        )?;
    }
    Ok(())
}

pub(super) fn validate_control_presentations(room: &AutoCommit) -> Result<(), String> {
    for (id, bytes) in super::blobs(room, crate::workspace_task::TASK_CONTROL_KEY)? {
        if manual_control(room, &id)? {
            continue;
        }
        let original: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut control = original.clone();
        neutralize_presentations(&mut control)?;
        if control != original {
            return Err("baseline contains private task presentation bodies".into());
        }
    }
    Ok(())
}

fn audit(task: &Value) -> Result<&[Value], String> {
    match task.get("audit") {
        None => Ok(&[]),
        Some(Value::Array(events)) => Ok(events),
        Some(_) => Err("task audit is not a list".into()),
    }
}

pub(super) fn neutralize_reports(task: &mut Value) -> Result<(), String> {
    if !automatic(task) {
        return Ok(());
    }
    // Validate before mutable traversal, including malformed audit containers.
    audit(task)?;
    if let Some(Value::Array(events)) = task.get_mut("audit") {
        for event in events {
            if event.get("note").is_none_or(Value::is_null) {
                continue;
            }
            let parsed: tilo_desktop_model::WorkspaceTaskAuditEvent =
                serde_json::from_value(event.clone()).map_err(|e| e.to_string())?;
            let neutral = match parsed.action() {
                "finish" => match event.pointer("/what/args/outcome").and_then(Value::as_str) {
                    Some("done") => "done",
                    Some("cancelled") => "cancelled",
                    _ => "waiting",
                },
                "abandon" => "abandoned",
                // Earlier audit-only progress and any other automatic notes must
                // not retain report prose merely because their verb is older.
                _ => "progress",
            };
            event["note"] = neutral.into();
        }
    }
    Ok(())
}
