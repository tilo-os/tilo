//! Exact Chat binding data, read from any admitted document.
use super::{CallPhaseV1, ChatCallBindingV1, MediaPolicyError, WorkspaceMediaV1};
use crate::workspace::read::{self, policy_error};
use automerge::{ObjId, ReadDoc};

/// The app whose voice channels hold the workspace's calls: Chat.
pub const CHAT_APP: &str = tilo_app_id::apps::CHAT;
/// The ids Chat's documents are stored under: its own, then the one it was published as before.
const CHAT_STORED_UNDER: [&str; 2] = [CHAT_APP, "so.tilo.lang.chat"];
/// The resource kind of Chat's voice channel.
pub const VOICE_CHANNEL_KIND: &str = "tl.so.tilo.chat.VoiceChannel";
/// The type id of [`VOICE_CHANNEL_KIND`], as each of Chat's items records its type.
pub const VOICE_CHANNEL_TYPE: &str = "xr347kui";
/// The key of the map in the document holding every tilolang resource, by app.
const RESOURCES_KEY: &str = "tl";
/// The key of the map in a resource holding its items by id.
const ITEMS_KEY: &str = "$items";
/// The key of an item's type id.
const TYPE_KEY: &str = "$type";
/// The key of an item's placement.
const AT_KEY: &str = "$at";
/// How many items above a channel are read for a removal.
const MAX_DEPTH: usize = 16;

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
/// The exact admitted Chat voice channel `channel` of `record`, as a call binds it.
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
    let (items, object) = channel_item(workspace, channel)?.ok_or(MediaPolicyError::Binding)?;
    if read::strict_string(workspace, &object, TYPE_KEY).map_err(policy_error)?
        != VOICE_CHANNEL_TYPE
        || removed(workspace, &items, &object)?
    {
        return Err(MediaPolicyError::Binding);
    }
    let mut buffer = [0; super::MAX_OBJECT_BYTES];
    let length = object_id(&object, &mut buffer)?.ok_or(MediaPolicyError::Binding)?;
    let object = buffer
        .get(..length)
        .and_then(|text| std::str::from_utf8(text).ok())
        .ok_or(MediaPolicyError::Malformed)?
        .to_owned();
    // Chat runs in its workspace: its channels name none of their own.
    let former = String::new();
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
/// Every map at `key` of `parent`: the one the document keeps and each copy a device made
/// before it met the others, in the same order on every replica.
fn maps(workspace: &impl ReadDoc, parent: &ObjId, key: &str) -> Vec<ObjId> {
    workspace
        .get_all(parent, key)
        .map(|held| {
            held.into_iter()
                .filter_map(|(value, object)| {
                    matches!(value, automerge::Value::Object(automerge::ObjType::Map))
                        .then_some(object)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The item `channel` of Chat's document, and the map of items holding it: Chat keeps every item
/// of a resource flat under the resource's items, each by its own id
/// (`tl/<app>/<resource>/$items/<item>`).
fn channel_item(
    workspace: &impl ReadDoc,
    channel: &str,
) -> Result<Option<(ObjId, ObjId)>, MediaPolicyError> {
    for records in maps(workspace, &automerge::ROOT, RESOURCES_KEY) {
        for app in CHAT_STORED_UNDER
            .iter()
            .flat_map(|stored| maps(workspace, &records, stored))
        {
            for resource in workspace.keys(&app) {
                for held in maps(workspace, &app, &resource) {
                    for items in maps(workspace, &held, ITEMS_KEY) {
                        match workspace.get(&items, channel) {
                            Ok(Some((
                                automerge::Value::Object(automerge::ObjType::Map),
                                object,
                            ))) => return Ok(Some((items, object))),
                            Ok(Some(_)) => return Err(MediaPolicyError::Binding),
                            Ok(None) => {}
                            Err(_) => return Err(MediaPolicyError::Malformed),
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Whether `item` of `items` is removed: its own placement says so, or the placement of an item
/// holding it does, as removing a category takes its channels with it.
fn removed(
    workspace: &impl ReadDoc,
    items: &ObjId,
    item: &ObjId,
) -> Result<bool, MediaPolicyError> {
    let mut at = item.clone();
    // A channel sits a level or two down (a category's channel); a longer chain is a cycle
    // concurrent moves made, which holds nothing a call can bind.
    for _ in 0..MAX_DEPTH {
        let placement = match read::strict_string(workspace, &at, AT_KEY) {
            Ok(text) => text,
            Err(read::ReadError::Visitor(MediaPolicyError::Missing)) => return Ok(false),
            Err(error) => return Err(policy_error(error)),
        };
        let placement: serde_json::Value =
            serde_json::from_str(&placement).map_err(|_| MediaPolicyError::Malformed)?;
        if placement
            .get("removed")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return Ok(true);
        }
        let Some(parent) = placement.get("in").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        at = match workspace.get(items, parent) {
            Ok(Some((automerge::Value::Object(automerge::ObjType::Map), object))) => object,
            // A holder that is not there holds nothing.
            Ok(_) => return Ok(true),
            Err(_) => return Err(MediaPolicyError::Malformed),
        };
    }
    Ok(true)
}

pub(super) fn validate_fields(
    app: &str,
    record: &str,
    channel: &str,
    object: &str,
    workspace: &str,
) -> Result<(), MediaPolicyError> {
    // A binding names an existing Chat record.
    if app != CHAT_APP
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
/// The id of Chat's conversations, the root resource its sidebar shows: `root-` and the type id
/// of its `Conversations`.
#[cfg(any(test, feature = "test-support"))]
pub const CONVERSATIONS: &str = "root-lmdoxbnn";

/// Chat's document with the voice channel `channel` named `name` at the top of its sidebar, as
/// Chat writes one: what a call binds, for a test of a call.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn voice_chat(channel: &str, name: &str) -> automerge::AutoCommit {
    let mut doc = automerge::AutoCommit::new();
    put_voice_channel(&mut doc, channel, name);
    doc
}

/// Write the voice channel `channel` named `name` into Chat's document `doc`, at the top of its
/// sidebar; its item.
///
/// # Panics
///
/// When `doc` cannot be written.
#[cfg(any(test, feature = "test-support"))]
#[expect(
    clippy::unwrap_used,
    reason = "a test's fixture: a document that takes no write fails the test"
)]
pub fn put_voice_channel(doc: &mut automerge::AutoCommit, channel: &str, name: &str) -> ObjId {
    use automerge::{ObjType, transaction::Transactable};
    let at = |doc: &mut automerge::AutoCommit, parent: &ObjId, key: &str| match doc.get(parent, key)
    {
        Ok(Some((automerge::Value::Object(ObjType::Map), object))) => object,
        _ => doc.put_object(parent, key, ObjType::Map).unwrap(),
    };
    let records = at(doc, &automerge::ROOT, RESOURCES_KEY);
    let app = at(doc, &records, CHAT_APP);
    let resource = at(doc, &app, CONVERSATIONS);
    let items = at(doc, &resource, ITEMS_KEY);
    let item = doc.put_object(&items, channel, ObjType::Map).unwrap();
    doc.put(&item, TYPE_KEY, VOICE_CHANNEL_TYPE).unwrap();
    doc.put(&item, "$app", CHAT_APP).unwrap();
    doc.put(
        &item,
        AT_KEY,
        format!(r#"{{"f":"channels","k":"{channel}"}}"#),
    )
    .unwrap();
    doc.put(&item, "name", name).unwrap();
    item
}
#[cfg(test)]
mod tests;
