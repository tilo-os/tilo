//! How a group's payload is written: its members, with what they share
//! written once.
//!
//! The members of one gesture are stamped together and usually edit the
//! same place, so a group of at least two members sharing a stamp is written
//! `{"at":…,"path":…,"ops":[…]}` (the path only when every field edit shares
//! it) instead of the list of its members. [`EncodedAction::encode`]
//! writes it so and [`readable`] restores the list before a payload is
//! decoded, on the encoded bytes: the group's members go through the same
//! JSON writer and reader as any action, and every replica decodes, mints
//! and replays the same.
//!
//! [`EncodedAction::encode`]: crate::EncodedAction::encode

use std::borrow::Cow;

/// What the type id of a group of a resource's actions ends with.
pub const GROUP_SUFFIX: &str = "+group";

/// Whether `type_id` names a group of actions.
#[must_use]
pub fn is_group(type_id: &str) -> bool {
    type_id.ends_with(GROUP_SUFFIX)
}

/// `payload`, a group's list of members, written with what they share once;
/// `None` when they share nothing to write once or it is not such a list.
#[must_use]
pub fn compact(payload: &[u8]) -> Option<Vec<u8>> {
    let serde_json::Value::Array(members) = serde_json::from_slice(payload).ok()? else {
        return None;
    };
    serde_json::to_vec(&shared_parts(&members)?).ok()
}

/// The members a group's parsed payload `group` stands for, whichever way it
/// was written; `None` for a payload that is no group.
#[must_use]
pub fn members(group: serde_json::Value) -> Option<Vec<serde_json::Value>> {
    match group {
        serde_json::Value::Array(members) => Some(members),
        serde_json::Value::Object(shared) => members_of(shared).ok(),
        _ => None,
    }
}

/// The payload of an action of type `type_id` as its type reads it: a
/// group written with what its members share once, as the list of them.
#[must_use]
pub fn readable<'a>(type_id: &str, payload: &'a [u8]) -> Cow<'a, [u8]> {
    if !is_group(type_id) || payload.first() != Some(&b'{') {
        return Cow::Borrowed(payload);
    }
    let restored = restored(payload).or_else(|| {
        serde_json::from_slice(payload)
            .ok()
            .and_then(|shared| members_of(shared).ok())
            .and_then(|members| serde_json::to_vec(&members).ok())
    });
    restored.map_or(Cow::Borrowed(payload), Cow::Owned)
}

/// A group written with what its members share, as the list of them,
/// restored on the JSON text: the members' operations are copied as they
/// are, only their arguments gain the shared path. `None` when the text
/// does not read that way (keys with escapes, say); [`members_of`] reads
/// it then.
fn restored(payload: &[u8]) -> Option<Vec<u8>> {
    use serde_json::value::RawValue;
    #[derive(serde::Deserialize)]
    struct Shared<'a> {
        #[serde(borrow)]
        at: &'a RawValue,
        #[serde(borrow, default)]
        path: Option<&'a RawValue>,
        #[serde(borrow)]
        ops: Vec<&'a RawValue>,
    }
    let shared: Shared<'_> = serde_json::from_slice(payload).ok()?;
    let mut out = Vec::with_capacity(payload.len().saturating_mul(2));
    out.push(b'[');
    for (index, op) in shared.ops.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"{\"op\":");
        match shared.path {
            Some(path) => with_path(op.get(), path, &mut out)?,
            None => out.extend_from_slice(op.get().as_bytes()),
        }
        out.extend_from_slice(b",\"at\":");
        out.extend_from_slice(shared.at.get().as_bytes());
        out.push(b'}');
    }
    out.push(b']');
    Some(out)
}

/// The operation `op` with `path` added to its arguments object, written
/// to `out`; an operation without one is copied as it is.
fn with_path(op: &str, path: &serde_json::value::RawValue, out: &mut Vec<u8>) -> Option<()> {
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    let fields: BTreeMap<&str, &RawValue> = serde_json::from_str(op).ok()?;
    let Some(args) = fields
        .get("args")
        .filter(|args| args.get().starts_with('{'))
    else {
        out.extend_from_slice(op.as_bytes());
        return Some(());
    };
    let mut args: BTreeMap<&str, &RawValue> = serde_json::from_str(args.get()).ok()?;
    args.insert("path", path);
    let mut args_text = Vec::new();
    write_object(&args, &mut args_text);
    let args_text = std::str::from_utf8(&args_text).ok()?;
    let args_raw: &RawValue = serde_json::from_str(args_text).ok()?;
    let mut fields = fields;
    fields.insert("args", args_raw);
    write_object(&fields, out);
    Some(())
}

/// `fields` as a JSON object; keys were read without escapes, so they are
/// written as they are.
fn write_object(
    fields: &std::collections::BTreeMap<&str, &serde_json::value::RawValue>,
    out: &mut Vec<u8>,
) {
    out.push(b'{');
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'"');
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(b"\":");
        out.extend_from_slice(value.get().as_bytes());
    }
    out.push(b'}');
}

/// `members` written with what they share once, when at least two share a
/// stamp (and, for their field edits, a path).
pub(crate) fn shared_parts(members: &[serde_json::Value]) -> Option<serde_json::Value> {
    if members.len() < 2 {
        return None;
    }
    let at = members.first()?.get("at")?;
    let mut ops = Vec::with_capacity(members.len());
    for member in members {
        let fields = member.as_object()?;
        if fields.len() != 2 || fields.get("at")? != at {
            return None;
        }
        ops.push(fields.get("op")?.clone());
    }
    let path = ops
        .first()
        .and_then(|op| op.get("args")?.get("path"))
        .cloned()
        .filter(|path| {
            ops.iter()
                .all(|op| op.get("args").and_then(|args| args.get("path")) == Some(path))
        });
    if path.is_some() {
        for op in &mut ops {
            if let Some(args) = op
                .get_mut("args")
                .and_then(serde_json::Value::as_object_mut)
            {
                args.remove("path");
            }
        }
    }
    let mut shared = serde_json::Map::new();
    shared.insert("at".into(), at.clone());
    if let Some(path) = path {
        shared.insert("path".into(), path);
    }
    shared.insert("ops".into(), serde_json::Value::Array(ops));
    Some(serde_json::Value::Object(shared))
}

/// The members a group written by [`shared_parts`] stands for.
pub(crate) fn members_of(
    mut shared: serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let at = shared
        .remove("at")
        .ok_or("a group's members share a stamp")?;
    let path = shared.remove("path");
    let Some(serde_json::Value::Array(ops)) = shared.remove("ops") else {
        return Err("a group lists its members".into());
    };
    Ok(ops
        .into_iter()
        .map(|mut op| {
            if let (Some(path), Some(args)) = (
                &path,
                op.get_mut("args")
                    .and_then(serde_json::Value::as_object_mut),
            ) {
                args.insert("path".into(), path.clone());
            }
            serde_json::json!({ "op": op, "at": at })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Members stamped together at one place are written with the stamp
    /// and the place once, and read back as exactly those members; a group
    /// sharing nothing, and any other action, stay as they are.
    #[test]
    fn a_group_writes_what_its_members_share_once_and_reads_back_whole() {
        let members = br#"[{"op":{"action":"field-edit","args":{"path":["cells"],"op":"set"}},"at":5},{"op":{"action":"field-edit","args":{"path":["cells"],"op":"clear"}},"at":5}]"#;
        let written =
            crate::EncodedAction::written("sheets.workbook+group".into(), members.to_vec());
        let text = String::from_utf8(written.payload.clone()).unwrap();
        assert!(text.starts_with(r#"{"at":5"#), "{text}");
        assert_eq!(text.matches("cells").count(), 1, "{text}");
        let read: serde_json::Value =
            serde_json::from_slice(&readable(&written.type_id, &written.payload)).unwrap();
        assert_eq!(
            read,
            serde_json::from_slice::<serde_json::Value>(members).unwrap()
        );

        for shared in [
            &br#"{"at":5,"ops":[{"action":"rename","args":{"to":"a"}},{"action":"move"}]}"#[..],
            br#"{"at":5,"path":["c"],"ops":[{"action":"edit","args":{"key":1}},{"action":"edit","args":[1]}]}"#,
        ] {
            let by_value = members_of(serde_json::from_slice(shared).unwrap()).unwrap();
            let read: serde_json::Value =
                serde_json::from_slice(&readable("sheets.workbook+group", shared)).unwrap();
            assert_eq!(read, serde_json::Value::Array(by_value));
        }

        let apart = br#"[{"op":{"action":"rename","args":{}},"at":5},{"op":{"action":"rename","args":{}},"at":6}]"#;
        let kept = crate::EncodedAction::written("notes.book+group".into(), apart.to_vec());
        assert_eq!(kept.payload, apart);
        let single = br#"{"at":5,"ops":[]}"#;
        assert_eq!(readable("notes.book", single), &single[..]);
    }
}
