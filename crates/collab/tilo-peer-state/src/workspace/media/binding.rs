//! Exact Chat binding data, read from any admitted document.
use super::{CallPhaseV1, ChatCallBindingV1, MediaPolicyError, WorkspaceMediaV1};
use crate::workspace::read::{self, Selection, WorkspaceRead, WorkspaceValue, policy_error};
use automerge::{ObjId, ReadDoc};

/// The text form of `object`'s id, written into `sink`; `None` when it does
/// not fit the binding's byte limit, which the caller refuses only after
/// reading the rest of the channel.
fn object_id(object: &ObjId, sink: &mut [u8]) -> Result<Option<usize>, MediaPolicyError> {
    let ObjId::Id(counter, actor, _) = object else {
        let Some(root) = sink.get_mut(..5) else {
            return Ok(None);
        };
        root.copy_from_slice(b"_root");
        return Ok(Some(5));
    };
    // A u64 has at most 20 decimal digits, so these steps are exact.
    let digits = counter
        .checked_ilog10()
        .map_or(1, |log| (log as usize).saturating_add(1));
    let Some(length) = actor
        .to_bytes()
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(digits.saturating_add(1)))
    else {
        return Ok(None);
    };
    let Some((number, rest)) = sink
        .get_mut(..length)
        .and_then(|target| target.split_at_mut_checked(digits))
    else {
        return Ok(None);
    };
    let Some((at, hex_digits)) = rest.split_first_mut() else {
        return Ok(None);
    };
    let mut value = *counter;
    for slot in number.iter_mut().rev() {
        *slot = b'0'.saturating_add((value % 10) as u8);
        value /= 10;
    }
    *at = b'@';
    hex::encode_to_slice(actor.to_bytes(), hex_digits).map_err(|_| MediaPolicyError::Malformed)?;
    Ok(Some(length))
}
/// The exact admitted Chat channel `channel` of `record`, as a call binds it.
/// A document missing changes it builds on binds nothing.
pub(super) fn resolve(
    app: &str,
    record: &str,
    channel: &str,
    workspace: &impl ReadDoc,
) -> Result<ChatCallBindingV1, MediaPolicyError> {
    if !workspace.get_missing_deps(&[]).is_empty() {
        return Err(MediaPolicyError::Binding);
    }
    // Chat keeps its channels as a collection: each entry holds its order
    // key and, under `v`, the channel itself. Two devices that made the same
    // channel (a workspace's first `#voice`) each created the collection, the
    // entry and its value; the document keeps every copy and every replica
    // reads the same winner at each level, as Chat itself does.
    let channels = winner_map(workspace, &automerge::ROOT, "channels")?;
    let entry = winner_map(workspace, &channels, channel)?;
    let object = winner_map(workspace, &entry, "v")?;
    if read::strict_string(workspace, &object, "kind").map_err(policy_error)? != "voice" {
        return Err(MediaPolicyError::Binding);
    }
    let count = workspace
        .visit_property(&object, "archived", Selection::All, |value| {
            if matches!(value, WorkspaceValue::Boolean(false)) {
                Ok(())
            } else {
                Err(MediaPolicyError::Binding)
            }
        })
        .map_err(policy_error)?;
    if count > 1 {
        return Err(MediaPolicyError::Binding);
    }
    let mut buffer = [0; super::MAX_OBJECT_BYTES];
    let length = object_id(&object, &mut buffer)?;
    let object_text = length
        .map(|length| {
            buffer
                .get(..length)
                .and_then(|text| std::str::from_utf8(text).ok())
                .ok_or(MediaPolicyError::Malformed)
        })
        .transpose()?;
    let object_copy = object_text.map(str::to_owned);
    let former = former_workspace(workspace, &object)?;
    let object = object_copy.ok_or(MediaPolicyError::Binding)?;
    validate_fields(app, record, channel, &object, &former)?;
    Ok(ChatCallBindingV1 {
        app_id: app.to_owned(),
        document_id: record.to_owned(),
        channel_id: channel.to_owned(),
        channel_object: object,
        chat_workspace_id: former,
    })
}
/// Every active call of `policy_value` bound to `app`/`record` still names
/// the channel it binds. The channel's object and the workspace it once
/// named are recorded, not compared: the object changes when two devices
/// made the same channel and the document keeps both copies, and Chat no
/// longer has workspaces of its own, so neither identifies it.
pub(super) fn validate(
    policy_value: &WorkspaceMediaV1,
    app: &str,
    record: &str,
    doc: &impl ReadDoc,
) -> Result<(), MediaPolicyError> {
    for call in policy_value.calls.values().filter(|call| {
        call.phase == CallPhaseV1::Active
            && call.binding.app_id == app
            && call.binding.document_id == record
    }) {
        if !resolve(app, record, &call.binding.channel_id, doc)?.same_channel(&call.binding) {
            return Err(MediaPolicyError::Binding);
        }
    }
    Ok(())
}
/// The winning map value of `key` under `parent`.
fn winner_map(
    workspace: &impl WorkspaceRead,
    parent: &ObjId,
    key: &str,
) -> Result<ObjId, MediaPolicyError> {
    let mut result = None;
    workspace
        .visit_property(parent, key, Selection::Winner, |value| match value {
            WorkspaceValue::Object {
                kind: automerge::ObjType::Map,
                object,
            } => {
                result = Some(object);
                Ok(())
            }
            _ => Err(MediaPolicyError::Binding),
        })
        .map_err(policy_error)?;
    result.ok_or(MediaPolicyError::Binding)
}

/// The workspace a channel of a chat that still had workspaces named, or
/// empty: Chat runs in its workspace and its channels name none. A value that is
/// present but no text is malformed.
fn former_workspace(
    workspace: &impl WorkspaceRead,
    object: &ObjId,
) -> Result<String, MediaPolicyError> {
    let mut result = None;
    workspace
        .visit_property(object, "workspace", Selection::All, |value| match value {
            WorkspaceValue::Str(text) if result.as_deref().is_none_or(|prior| prior == text) => {
                result = Some(text.to_owned());
                Ok(())
            }
            WorkspaceValue::Str(_) => Err(MediaPolicyError::Conflict),
            _ => Err(MediaPolicyError::Malformed),
        })
        .map_err(policy_error)?;
    Ok(result.unwrap_or_default())
}

pub(super) fn validate_fields(
    app: &str,
    record: &str,
    channel: &str,
    object: &str,
    workspace: &str,
) -> Result<(), MediaPolicyError> {
    // A binding names an existing Chat record.
    if app != "so.tilo.chat"
        || !super::text_valid(record, super::MAX_DOCUMENT_BYTES, true)
        || !super::text_valid(channel, super::MAX_CHANNEL_BYTES, false)
        || !super::text_valid(workspace, super::MAX_CHANNEL_BYTES, true)
        || !super::text_valid(object, super::MAX_OBJECT_BYTES, false)
    {
        return Err(MediaPolicyError::Binding);
    }
    let (counter, actor) = object.split_once('@').ok_or(MediaPolicyError::Binding)?;
    if !counter
        .as_bytes()
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
        || !counter.bytes().all(|byte| byte.is_ascii_digit())
        || counter.parse::<u64>().is_err()
        || actor.is_empty()
        || !actor.len().is_multiple_of(2)
        || !super::lower_hex(actor)
    {
        return Err(MediaPolicyError::Binding);
    }
    Ok(())
}
#[cfg(test)]
mod tests;
