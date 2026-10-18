//! Patch only private task leaves, then copy current values into fresh history.
use crate::workspace_task::{NativeTaskPrivateReferences, NativeTaskPrivateText, TASK_CONTROL_KEY};
use automerge::{
    AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use std::collections::BTreeMap;

#[cfg(test)]
mod confirmation_tests;
mod confirmations;
#[cfg(test)]
mod presentation_tests;
mod reports;
#[cfg(test)]
mod tests;

pub(super) fn sanitize_task_room(
    room: &AutoCommit,
    references: &NativeTaskPrivateReferences,
) -> Result<(AutoCommit, BTreeMap<String, NativeTaskPrivateText>), String> {
    let mut visible = room.clone();
    let tasks_key = tilo_desktop_model::projection::ROOM_TASKS_KEY;
    let tasks = blobs(room, tasks_key)?;
    let controls = blobs(room, TASK_CONTROL_KEY)?;
    let legacy = super::super::sanitized_baseline::binding(room)?.is_none();
    // Recover reports before dropping history, including overwritten/deleted
    // task values. These remain private archival events, not invented receipts.
    let mut extracted = reports::historical_reports(room)?;
    for (id, bytes) in tasks {
        let Ok(mut task) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            // Unknown entries are not schema-owned tasks; retain their bytes.
            continue;
        };
        let route = task.get("route").and_then(serde_json::Value::as_str);
        if !matches!(route, Some("model" | "rules_only")) {
            confirmations::neutralize_task(&mut task)?;
            put_blob(&mut visible, tasks_key, &id, &bytes, &task)?;
            continue;
        }
        let model = route == Some("model");
        if let Some(control_map) = map(room, TASK_CONTROL_KEY)?
            && room
                .get(control_map, &id)
                .map_err(|e| e.to_string())?
                .is_some()
            && !controls.contains_key(&id)
        {
            return Err("managed task control is not bytes".into());
        }
        let mut private = NativeTaskPrivateText::default();
        if let Some(resource) = task
            .get_mut("resource")
            .and_then(serde_json::Value::as_object_mut)
            && let Some(request) = resource.remove("agent_request")
            && !request.is_null()
        {
            private.agent_request =
                Some(serde_json::from_value(request).map_err(|e| e.to_string())?);
        }
        let default_control =
            serde_json::to_vec(&tilo_desktop_model::WorkspaceTaskControl::default())
                .map_err(|e| e.to_string())?;
        if let Some(bytes) = controls
            .get(&id)
            .or_else(|| private.agent_request.as_ref().map(|_| &default_control))
        {
            let control = sanitize_control(bytes, &id, &mut private, references)?;
            if controls.contains_key(&id) {
                put_blob(&mut visible, TASK_CONTROL_KEY, &id, bytes, &control)?;
            } else {
                let controls_map = match map(&visible, TASK_CONTROL_KEY)? {
                    Some(map) => map,
                    None => visible
                        .put_object(ROOT, TASK_CONTROL_KEY, ObjType::Map)
                        .map_err(|e| e.to_string())?,
                };
                visible
                    .put(
                        controls_map,
                        &id,
                        ScalarValue::Bytes(
                            serde_json::to_vec(&control).map_err(|e| e.to_string())?,
                        ),
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        confirmations::neutralize_task(&mut task)?;
        reports::neutralize_reports(&mut task)?;
        let affected = private.instruction.is_some()
            || private.agent_request.is_some()
            || !private.directions.is_empty();
        let instruction = private.instruction.as_deref().or_else(|| {
            private
                .agent_request
                .as_ref()
                .map(|request| request.instruction.as_str())
        });
        if legacy
            && model
            && let Some(instruction) = instruction
        {
            neutralize_legacy_title(&mut task, instruction)?;
        }
        put_blob(&mut visible, tasks_key, &id, &bytes, &task)?;
        if affected {
            let retained = extracted.entry(id).or_default();
            retained.instruction = private.instruction;
            retained.agent_request = private.agent_request;
            retained.directions.extend(private.directions);
        }
    }
    // Model/account configuration is private regardless of task route. Extract
    // current custody, then rebuild history without any old profile bytes.
    for (id, bytes) in blobs(&visible, TASK_CONTROL_KEY)? {
        let mut control: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(model) = control
            .get_mut("model_authority")
            .filter(|model| !model.is_null())
        {
            let full: tilo_desktop_model::task::WorkspaceTaskModelAuthority =
                serde_json::from_value(model.clone()).map_err(|e| e.to_string())?;
            if !crate::workspace_task::is_private_model_contract_reference(&full.contract) {
                let reference =
                    crate::workspace_task::private_model_contract_reference(&full.contract)?;
                extracted.entry(id.clone()).or_default().model_authority = Some(full);
                model["contract"] = reference;
                put_blob(&mut visible, TASK_CONTROL_KEY, &id, &bytes, &control)?;
            }
        }
    }
    reports::scrub_control_presentations(&mut visible)?;
    confirmations::scrub_controls(&mut visible, &mut extracted)?;
    visible.commit();
    // Crucially, no historical change or secret-containing transaction is copied.
    let candidate = super::super::sanitized_baseline::rebuild(&visible)?;
    Ok((candidate, extracted))
}

pub(super) fn validate_reports(room: &AutoCommit) -> Result<(), String> {
    for (_, bytes) in blobs(room, TASK_CONTROL_KEY)? {
        let control: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(model) = control
            .get("model_authority")
            .filter(|model| !model.is_null())
            && !model
                .get("contract")
                .is_some_and(crate::workspace_task::is_private_model_contract_reference)
        {
            return Err("baseline contains private model authority contract".into());
        }
    }
    reports::validate_control_presentations(room)?;
    confirmations::validate_controls(room)?;
    for (_, bytes) in blobs(room, tilo_desktop_model::projection::ROOM_TASKS_KEY)? {
        let Ok(mut task) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let original = task.clone();
        confirmations::neutralize_task(&mut task)?;
        reports::neutralize_reports(&mut task)?;
        if task != original {
            return Err("baseline contains private automatic task reports".into());
        }
    }
    Ok(())
}

fn sanitize_control(
    bytes: &[u8],
    id: &str,
    private: &mut NativeTaskPrivateText,
    references: &NativeTaskPrivateReferences,
) -> Result<serde_json::Value, String> {
    let mut control: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let fields = control
        .as_object_mut()
        .ok_or("task control is not an object")?;
    if let Some(instruction) = fields.remove("instruction")
        && !instruction.is_null()
    {
        private.instruction = Some(
            instruction
                .as_str()
                .ok_or("task instruction is not text")?
                .to_owned(),
        );
    }
    if let Some(instruction) = private.instruction.as_deref().or_else(|| {
        private
            .agent_request
            .as_ref()
            .map(|request| request.instruction.as_str())
    }) {
        let reference = crate::workspace_task::private_instruction_reference(instruction);
        if private.agent_request.as_ref().is_some_and(|request| {
            crate::workspace_task::private_instruction_reference(&request.instruction) != reference
        }) || fields
            .get("private_instruction_ref")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|old| old != reference)
        {
            return Err("private task instruction binding changed".into());
        }
        fields.insert("private_instruction_ref".into(), reference.into());
    }
    if let Some(directions) = fields.get_mut("directions") {
        for direction in directions
            .as_array_mut()
            .ok_or("task directions are not a list")?
        {
            let revision = direction
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .ok_or("task direction revision absent")?;
            let text = direction
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or("task direction text absent")?
                .to_owned();
            let known = references.get(id).and_then(|values| values.get(&revision));
            // A reference-shaped string alone is not proof that this
            // revision was previously extracted into private storage.
            if known != Some(&text) {
                if private.directions.insert(revision, text.clone()).is_some() {
                    return Err("duplicate private direction revision".into());
                }
                direction["text"] = serde_json::Value::String(
                    crate::workspace_task::private_instruction_reference(&text),
                );
            }
        }
    }
    Ok(control)
}

fn neutralize_legacy_title(task: &mut serde_json::Value, instruction: &str) -> Result<(), String> {
    let fields = task
        .as_object_mut()
        .ok_or("managed task is not an object")?;
    if let Some(title) = fields.get_mut("title") {
        neutralize_title_value(title, instruction);
    }
    if let Some(audit) = task
        .get_mut("audit")
        .and_then(serde_json::Value::as_array_mut)
    {
        for event in audit {
            if let Some(what) = event.get_mut("what") {
                neutralize_audit_titles(what, instruction);
            }
        }
    }
    Ok(())
}

fn neutralize_audit_titles(what: &mut serde_json::Value, instruction: &str) {
    if let Some(title) = what.pointer_mut("/args/title") {
        neutralize_title_value(title, instruction);
    }
    if let Some(members) = what
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in members {
            neutralize_audit_titles(member, instruction);
        }
    }
    if let Some(previous) = what.pointer_mut("/of/what") {
        neutralize_audit_titles(previous, instruction);
    }
}

fn neutralize_title_value(value: &mut serde_json::Value, instruction: &str) {
    if value
        .as_str()
        .is_some_and(|title| crate::workspace_task::legacy_private_task_title(title, instruction))
    {
        *value = "Model task".into();
    }
}

fn blobs(doc: &AutoCommit, field: &str) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let Some(map) = map(doc, field)? else {
        return Ok(BTreeMap::new());
    };
    let mut blobs = BTreeMap::new();
    for key in doc.keys(&map) {
        if let Some((Value::Scalar(value), _)) = doc.get(&map, &key).map_err(|e| e.to_string())?
            && let ScalarValue::Bytes(bytes) = value.as_ref()
        {
            blobs.insert(key, bytes.clone());
        }
    }
    Ok(blobs)
}
fn map(doc: &AutoCommit, field: &str) -> Result<Option<ObjId>, String> {
    match doc.get(ROOT, field).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        _ => Err("task root is not a map".into()),
    }
}
fn put_blob(
    doc: &mut AutoCommit,
    field: &str,
    id: &str,
    old: &[u8],
    value: &serde_json::Value,
) -> Result<(), String> {
    // Preserve unaffected blobs byte-for-byte, including formatting and extras.
    let original: serde_json::Value = serde_json::from_slice(old).map_err(|e| e.to_string())?;
    if original == *value {
        return Ok(());
    }
    let map = map(doc, field)?.ok_or("task map absent")?;
    doc.put(
        map,
        id,
        ScalarValue::Bytes(serde_json::to_vec(value).map_err(|e| e.to_string())?),
    )
    .map_err(|e| e.to_string())
}
