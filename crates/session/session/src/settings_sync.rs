//! The person's settings following them to their other devices.
//!
//! The sections that are the person's ([`settings_store::placement`]) are one resource of the
//! workspace whose only member is them, which every device of theirs takes part in
//! ([`persons_workspace`]): each leaf (`tilo.<section>.<leaf>`, an object under a leaf travelling
//! as one value) under its key. A write of one goes to that resource first ([`Record`], the
//! settings' [`settings_store::RoamingSettings`]), and the settings document keeps a copy every
//! process reads at once; what another of their devices wrote there is taken into that copy as it
//! arrives — so the theme, the language or an app's preference chosen on the laptop is what this
//! desktop reads next, and nothing of this machine's (`app_device_local`, the clock, the
//! display) ever leaves it.
//!
//! The resource is kept by the session's copies of its workspaces' resources
//! ([`EngineRecords::keep_document`]), which merge what each device wrote, a later write of one
//! setting winning over an earlier one.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ReadDoc as _};
use collab_types::CommitNamed as _;
use serde_json::{Map, Value};
use settings_store::person_displays::{PLACEMENTS_KEY, Placements};
use settings_store::placement::Holder;

use crate::engine_records::{EngineRecords, KeptDoc};

/// The person's settings as a resource of their workspace: the settings app's.
pub(crate) const RECORD: (&str, &str) = ("so.tilo.settings", "person");

/// The kind the changes of the person's settings are named as.
const CHANGE_KIND: &str = "settings";

/// The workspace whose only member is the person, which every device of theirs takes part in
/// from sign-in.
pub(crate) fn persons_workspace(person: &[u8; 32]) -> String {
    tilo_server_protocol::servers::person_workspace_id(person)
}

/// The person's record, as the settings write to it: every write of a setting the person holds
/// lands here first, and the session's loop is woken to carry it to the workspace.
pub(crate) struct Record {
    doc: Arc<Mutex<AutoCommit>>,
    wake: tilo_service::Wake,
    /// The person the settings are: who every write of them is by.
    person: [u8; 32],
}

impl settings_store::RoamingSettings for Record {
    fn write(
        &self,
        leaves: &[(String, String, Value)],
        _origin: &settings_store::ChangeOrigin,
    ) -> Result<(), String> {
        let mut doc = self.doc.lock().unwrap_or_else(PoisonError::into_inner);
        for (section, leaf, value) in leaves {
            let key = format!("tilo.{section}.{leaf}");
            if value.is_null() {
                if doc
                    .get(automerge::ROOT, key.as_str())
                    .ok()
                    .flatten()
                    .is_some()
                {
                    doc.delete(automerge::ROOT, key.as_str())
                        .map_err(|error| error.to_string())?;
                }
            } else {
                // Each display's place is its own: what another device placed meanwhile stays.
                let value = match values(&doc).get(&key) {
                    Some(theirs) if key == PLACEMENTS_KEY => placements([theirs, value]),
                    _ => value.clone(),
                };
                let text = serde_json::to_string(&value).map_err(|error| error.to_string())?;
                if text.len() > settings_store::placement::MAX_PERSONAL_VALUE_BYTES {
                    doc.rollback();
                    return Err(format!("{key} is too large for the person's devices"));
                }
                doc.put(automerge::ROOT, key.as_str(), text)
                    .map_err(|error| error.to_string())?;
            }
            doc.commit_edit_as(
                CHANGE_KIND,
                &key,
                Some(&tilo_actions::ActionInvoker::person(
                    tilo_action_policy::PersonId::from_root(self.person),
                )),
            );
        }
        drop(doc);
        (self.wake)();
        Ok(())
    }
}

/// The person's settings, kept in step with their other devices.
pub(crate) struct SettingsSync {
    /// The person's record, as the session's copies of its workspace's records keep it.
    record: Arc<Mutex<AutoCommit>>,
    /// The workspace the record is kept in, once it was handed to the records.
    kept: RefCell<Option<String>>,
    /// The record's heads as the settings document last took them in: `None` before the first
    /// look, which also offers what this device kept before it had the record.
    seen: RefCell<Option<Vec<ChangeHash>>>,
}

impl SettingsSync {
    pub(crate) fn new() -> Self {
        Self {
            record: Arc::new(Mutex::new(AutoCommit::new())),
            kept: RefCell::new(None),
            seen: RefCell::new(None),
        }
    }

    /// Keep the person's record in `records` once this device takes part in their workspace, writing
    /// the settings the person holds there from then on, and take into the settings document what
    /// the record holds as it moves. Returns whether the settings here changed.
    pub(crate) fn sync(&self, records: &EngineRecords, person: &[u8; 32]) -> bool {
        let Some(source) = settings_store::source() else {
            return false;
        };
        let Some(store) = source.store() else {
            return false;
        };
        let workspace = persons_workspace(person);
        if self.kept.borrow().as_deref() != Some(workspace.as_str()) {
            if !records.holds(&workspace) {
                return false;
            }
            records.keep_document(
                &workspace,
                RECORD,
                KeptDoc::shared(Arc::clone(&self.record)),
            );
            store.keep_roaming_in(Some(Arc::new(Record {
                doc: Arc::clone(&self.record),
                wake: records.wake(),
                person: *person,
            })));
            *self.kept.borrow_mut() = Some(workspace.clone());
            *self.seen.borrow_mut() = None;
        }
        if !records.kept_read(&workspace, RECORD) {
            return false;
        }
        let heads = self.lock().get_heads();
        if self.seen.borrow().as_ref() == Some(&heads) {
            return false;
        }
        let document = settings_store::load();
        if self.seen.borrow().is_none() {
            // What this device kept before it had the person's record (chosen as it was set up,
            // or before it held the workspace) is the person's where the record holds nothing of
            // it yet.
            let offered = offer(&mut self.lock(), &document, person);
            if offered {
                records.write();
            }
        }
        let theirs = values(&self.lock());
        let changed = take(store, &document, &theirs);
        let heads = self.lock().get_heads();
        *self.seen.borrow_mut() = Some(heads);
        changed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AutoCommit> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Write into `doc` each setting `document` holds that the person holds and the record does not.
/// Whether anything was written.
fn offer(doc: &mut AutoCommit, document: &Map<String, Value>, person: &[u8; 32]) -> bool {
    let stored = values(doc);
    let mut wrote = false;
    for (key, value) in personal_leaves(document) {
        if stored.contains_key(&key) {
            continue;
        }
        let Ok(text) = serde_json::to_string(&value) else {
            continue;
        };
        if text.len() > settings_store::placement::MAX_PERSONAL_VALUE_BYTES {
            tracing::warn!(%key, "a setting is too large for the person's devices");
            continue;
        }
        if let Err(error) = doc.put(automerge::ROOT, key.as_str(), text) {
            tracing::warn!(%error, %key, "a setting did not reach the person's record");
            continue;
        }
        doc.commit_edit_as(
            CHANGE_KIND,
            &key,
            Some(&tilo_actions::ActionInvoker::person(
                tilo_action_policy::PersonId::from_root(*person),
            )),
        );
        wrote = true;
    }
    wrote
}

/// Take into the settings document every setting `theirs` holds that it does not: what the person
/// set on their other devices. Returns whether the document changed.
fn take(
    store: &settings_store::Store,
    document: &Map<String, Value>,
    theirs: &BTreeMap<String, Value>,
) -> bool {
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
    if sections.is_empty() {
        return false;
    }
    let sections = sections
        .into_iter()
        .map(|(section, values)| (section.to_owned(), values))
        .collect();
    match store.take_roaming(sections) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "what the person's other devices set was not kept");
            false
        }
    }
}

/// The settings the record `doc` holds, by key. Two devices that wrote one setting at once
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
