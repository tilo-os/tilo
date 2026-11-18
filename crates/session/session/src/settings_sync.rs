//! The person's settings following them to their other devices.
//!
//! A profile keeps two documents: the sections the person holds, which follow them, and this
//! device's, which stay here ([`settings_store::placement`]). The person's are carried by their own
//! store, as one of its resources ([`tilo_peer_state::SETTINGS_KIND`]): each leaf the person holds
//! (`tilo.<section>.<leaf>`, an object under a leaf travelling as one value) under its key. What
//! the person changes here is written there, and what another of their devices wrote there is
//! taken into the document — so the theme, the language or an app's preference chosen on the
//! laptop is what this desktop reads next, and nothing of this machine's (`app_device_local`, the
//! clock, the display) ever leaves it.
//!
//! The store merges what each device wrote, a later write of one setting winning over an earlier
//! one, and admits a write only from a device the person's root certified. This device writes only
//! what changed here since it last agreed with the store, so a setting another device changed
//! meanwhile is taken in rather than written over.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _};
use collab_types::CommitNamed as _;
use serde_json::{Map, Value};
use settings_store::person_displays::{PLACEMENTS_KEY, Placements};
use settings_store::placement::Holder;
use tilo_peer_state::{PersonalSync, SETTINGS_KIND};

/// The kind the changes of the person's settings are named as.
const CHANGE_KIND: &str = "settings";

/// The person's settings, kept in step with their other devices.
pub(crate) struct SettingsSync {
    /// The settings document and the person's store as last looked at: while neither moved, there
    /// is nothing to write or to take.
    seen: Cell<Option<(Option<settings_store::SettingsStamp>, u64)>>,
    /// Each setting as this device and the person's store last agreed it; `None` before the first
    /// pass, which takes what the store holds over what this device kept.
    agreed: RefCell<Option<BTreeMap<String, Value>>>,
}

impl SettingsSync {
    pub(crate) fn new() -> Self {
        Self {
            seen: Cell::new(None),
            agreed: RefCell::new(None),
        }
    }

    /// Write what the person changed here to their store, and take what their other devices
    /// changed there. Returns whether the settings here changed.
    pub(crate) fn sync(&self, personal: &PersonalSync) -> bool {
        let looked = (settings_store::stamp(), *personal.changes().borrow());
        if self.seen.get() == Some(looked) {
            return false;
        }
        let document = settings_store::load();
        let held = personal.read(|store| store.person_resource(SETTINGS_KIND));
        let mut doc = match held.as_deref().map(AutoCommit::load) {
            Some(Ok(doc)) => doc,
            Some(Err(error)) => {
                tracing::warn!(%error, "the person's settings in their store do not read");
                return false;
            }
            None => AutoCommit::new(),
        };
        let mut agreed = self.agreed.borrow_mut();
        let first = agreed.is_none();
        let agreed = agreed.get_or_insert_with(|| values(&doc));
        if let Err(error) =
            Self::write(&mut doc, held.is_some(), &document, agreed, first, personal)
        {
            tracing::warn!(%error, "the person's settings did not reach their store");
        }
        let theirs = values(&doc);
        let changed = take(&document, &theirs);
        *agreed = theirs;
        self.seen.set(Some((
            settings_store::stamp(),
            *personal.changes().borrow(),
        )));
        changed
    }

    /// Write each setting the person changed here since this device last agreed with their store
    /// into `doc` (the store's copy), and publish the change. On the first pass only what the store
    /// does not hold yet is written: a device that comes up takes the person's settings.
    fn write(
        doc: &mut AutoCommit,
        held: bool,
        document: &Map<String, Value>,
        agreed: &BTreeMap<String, Value>,
        first: bool,
        personal: &PersonalSync,
    ) -> Result<(), String> {
        let stored = values(doc);
        let base = doc.get_heads();
        let mut wrote = false;
        for (key, value) in personal_leaves(document) {
            if stored.get(&key) == Some(&value)
                || (first && stored.contains_key(&key))
                || (!first && agreed.get(&key) == Some(&value))
            {
                continue;
            }
            // Each display's place is its own: what another device placed meanwhile stays.
            let value = match stored.get(&key) {
                Some(theirs) if key == PLACEMENTS_KEY => placements([theirs, &value]),
                _ => value,
            };
            if stored.get(&key) == Some(&value) {
                continue;
            }
            let text = serde_json::to_string(&value).map_err(|error| error.to_string())?;
            if text.len() > settings_store::placement::MAX_PERSONAL_VALUE_BYTES {
                tracing::warn!(%key, "a setting is too large for the person's devices");
                continue;
            }
            doc.put(automerge::ROOT, key.as_str(), text)
                .map_err(|error| error.to_string())?;
            doc.commit_edit(CHANGE_KIND, &key);
            wrote = true;
        }
        if !wrote {
            return Ok(());
        }
        let mutation = if held {
            collab_types::DocumentMutation::Incremental(doc.save_after(&base))
        } else {
            collab_types::DocumentMutation::Snapshot(doc.save())
        };
        personal
            .publish(|store, key, now| {
                store.publish_person_resource(SETTINGS_KIND, &mutation, key, now)
            })
            .map(drop)
    }
}

/// Take into the document every setting `theirs` holds that it does not: what the person set on
/// their other devices. Returns whether the document changed.
fn take(document: &Map<String, Value>, theirs: &BTreeMap<String, Value>) -> bool {
    let mut sections = BTreeMap::<&str, Map<String, Value>>::new();
    for (key, value) in theirs {
        let Some((section, leaf)) = key
            .strip_prefix("tilo.")
            .and_then(|rest| rest.split_once('.'))
        else {
            continue;
        };
        // Only a section the person holds is theirs to set from another device.
        if settings_store::placement::holder(section) != Holder::Person
            || leaf_of(document, key) == Some(value)
        {
            continue;
        }
        sections
            .entry(section)
            .or_default()
            .insert(leaf.to_owned(), value.clone());
    }
    let mut changed = false;
    for (section, values) in sections {
        match settings_store::update_section(section, values) {
            Ok(()) => changed = true,
            Err(error) => {
                tracing::warn!(%error, section, "a setting from another device was not kept");
            }
        }
    }
    changed
}

/// The settings the store's copy `doc` holds, by key. Two devices that wrote one setting at once
/// settle on one of their values, except for where the person's displays are placed: each display
/// keeps its newest place, whichever device wrote it.
fn values(doc: &AutoCommit) -> BTreeMap<String, Value> {
    doc.keys(automerge::ROOT)
        .filter_map(|key| {
            let read = |value: automerge::Value<'_>| -> Option<Value> {
                tilo_failure::ignored!(
                    "a value no build of this person's wrote is no setting of theirs",
                    serde_json::from_str(&value.into_string().ok()?).ok()
                )
            };
            if key == PLACEMENTS_KEY {
                let written: Vec<Value> = doc
                    .get_all(automerge::ROOT, key.as_str())
                    .ok()?
                    .into_iter()
                    .filter_map(|(value, _)| read(value))
                    .collect();
                return Some((key, placements(&written)));
            }
            let (value, _) = doc.get(automerge::ROOT, key.as_str()).ok()??;
            Some((key, read(value)?))
        })
        .collect()
}

/// The placements `written` hold, merged display by display.
fn placements<'a>(written: impl IntoIterator<Item = &'a Value>) -> Value {
    Placements::merge(
        written
            .into_iter()
            .filter_map(Placements::from_value)
            .map(|placement| ("", placement)),
    )
    .to_value()
}

/// Every leaf of the sections the person holds, by its key (`tilo.<section>.<leaf>`). An object
/// below a leaf travels as one value.
fn personal_leaves(document: &Map<String, Value>) -> BTreeMap<String, Value> {
    let mut leaves = BTreeMap::new();
    for (section, values) in document {
        if settings_store::placement::holder(section) != Holder::Person {
            continue;
        }
        let Value::Object(values) = values else {
            continue;
        };
        for (leaf, value) in values {
            leaves.insert(format!("tilo.{section}.{leaf}"), value.clone());
        }
    }
    leaves
}

/// The value `document` holds at the key `key` .
fn leaf_of<'a>(document: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let (section, leaf) = key.strip_prefix("tilo.")?.split_once('.')?;
    document.get(section)?.as_object()?.get(leaf)
}

#[cfg(test)]
#[path = "settings_sync_tests.rs"]
mod tests;
