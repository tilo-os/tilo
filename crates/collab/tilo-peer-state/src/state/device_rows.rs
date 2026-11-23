//! Rows each device writes for itself in a map the workspace is founded with.
//!
//! A map put at one document key by two devices that cannot see each other is
//! two maps, and a merge keeps only one of them: every row the other device
//! wrote into its own map is gone. So the maps that devices' rows sit in are
//! made once, when the workspace is founded ([`super::seed`]), and nothing
//! makes them later. A device's row sits straight in that map under
//! `<scope>/<device key hex>`, so the first rows two devices write for one
//! scope are two keys of one map and a merge keeps both.

use super::SystemTime;
use super::facades::PeerWorkspaceMut;
use super::{DONE_KEY, HELD_COPIES_KEY, LEASES_KEY, ROOT, SCHEDULES_KEY};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ReadDoc as _, ScalarValueRef, Value, ValueRef};

/// The maps of device rows every workspace is founded with.
pub(super) const FOUNDED: [&str; 4] = [LEASES_KEY, DONE_KEY, SCHEDULES_KEY, HELD_COPIES_KEY];

/// The key of `device`'s row under `scope`.
pub(super) fn row_key(scope: &str, device: &[u8; 32]) -> String {
    format!("{scope}/{}", hex::encode(device))
}

/// The key of `device`'s row in `person`'s name: a slice of a person's
/// devices, each writing its own row.
pub(super) fn person_row_key(person: &[u8; 32], device: &[u8; 32]) -> String {
    format!("{}/{}", hex::encode(person), hex::encode(device))
}

/// The person and device a [`person_row_key`] names, when `key` is one.
pub(super) fn person_and_device(key: &str) -> Option<(String, [u8; 32])> {
    let (person, device) = key.split_once('/')?;
    Some((person.to_owned(), device_of(device)?))
}

/// The key a device row's hex names, if it is one.
pub(super) fn device_of(device: &str) -> Option<[u8; 32]> {
    let key = tilo_failure::ignored!(
        "a device row another build wrote is not a key this one can name",
        hex::decode(device).ok()
    )?;
    <[u8; 32]>::try_from(key.as_slice()).ok()
}

/// Every row of `map` in `doc` whose key starts with `prefix`, as the rest of
/// its key and the text it holds.
fn texts(doc: &AutoCommit, map: &str, prefix: &str) -> Vec<(String, String)> {
    let Ok(Some((Value::Object(ObjType::Map), rows))) = doc.get(ROOT, map) else {
        return Vec::new();
    };
    doc.map_range(&rows, prefix.to_owned()..)
        .map_while(|item| {
            let rest = item.key.strip_prefix(prefix)?.to_owned();
            Some((rest, item.value))
        })
        .filter_map(|(rest, value)| match value {
            ValueRef::Scalar(ScalarValueRef::Str(text)) => Some((rest, text.into_owned())),
            _ => None,
        })
        .collect()
}

/// Every device's row under `scope` in `map`, as its device and its text.
pub(super) fn rows_of(doc: Option<&AutoCommit>, map: &str, scope: &str) -> Vec<([u8; 32], String)> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    let prefix = format!("{scope}/");
    texts(doc, map, &prefix)
        .into_iter()
        .filter(|(device, _)| !device.contains('/'))
        .filter_map(|(device, text)| Some((device_of(&device)?, text)))
        .collect()
}

/// Every row in `map`, as its scope, its device and its text.
pub(super) fn all_rows(doc: Option<&AutoCommit>, map: &str) -> Vec<(String, [u8; 32], String)> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    texts(doc, map, "")
        .into_iter()
        .filter_map(|(key, text)| {
            let (scope, device) = key.rsplit_once('/')?;
            Some((scope.to_owned(), device_of(device)?, text))
        })
        .collect()
}

impl PeerWorkspaceMut<'_> {
    /// Put `text` at `key` of the founded `map`, or remove the row when `text`
    /// is `None`, as `device`'s own change, made at `now`. Answers whether a row was there.
    ///
    /// # Errors
    ///
    /// The workspace or its `map` is absent, or the row cannot be written.
    pub(super) fn write_row(
        &mut self,
        map: &str,
        key: &str,
        device: &[u8; 32],
        text: Option<&str>,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.set_row(
            map,
            key,
            device,
            text.map(|text| automerge::ScalarValue::Str(text.into())),
            now,
        )
    }

    /// Put `value` at `key` of the founded `map`, or remove the row when
    /// `value` is `None`, as `device`'s own change, made at `now`
    /// ([`super::commit_governance`]). Answers whether a row was there.
    ///
    /// # Errors
    ///
    /// The workspace or its `map` is absent, or the row cannot be written.
    pub(super) fn set_row(
        &mut self,
        map: &str,
        key: &str,
        device: &[u8; 32],
        value: Option<automerge::ScalarValue>,
        now: SystemTime,
    ) -> Result<bool, String> {
        let actor = self.peer.workspace_actor_for(device);
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("the workspace is absent")?;
        let Ok(Some((Value::Object(ObjType::Map), rows))) = workspace.get(ROOT, map) else {
            return Err(format!("the workspace was founded without its `{map}`"));
        };
        let held = workspace.get(&rows, key).map_err(message)?.is_some();
        if value.is_none() && !held {
            return Ok(false);
        }
        let mut candidate = workspace.clone();
        candidate.set_actor(actor);
        match value {
            Some(value) => candidate.put(&rows, key, value).map_err(message)?,
            None => candidate.delete(&rows, key).map_err(message)?,
        }
        super::commit_governance(&mut candidate, super::now_secs(now));
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(held)
    }
}

/// A document operation's error as text.
fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}
