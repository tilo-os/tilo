//! The person's settings following them to their other devices.
//!
//! A profile keeps two documents: the sections the person holds, which
//! follow them, and this device's, which stay here ([`settings_store::placement`]).
//! This mount carries the person's to the peer fabric as this device's own
//! records, and takes what the person's other devices set back into the
//! document — so the theme, the language or an app's preference chosen on the
//! laptop is what this desktop reads next, and nothing of this machine's
//! (`app_device_local`, the clock, the display) ever leaves it.
//!
//! Every pass offers this device's record of each leaf the person holds
//! (`tilo.<section>.<leaf>`, an object under a leaf travelling as one value).
//! A value that did not change keeps the stamp it was offered with, so a
//! setting written days ago does not outrank a change another device made
//! since, and a value this device took from the fabric is not offered back
//! under a newer stamp: the device the person wrote it on stays its author.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use settings_store::placement::Holder;
use tilo_device_fabric::link::FabricLink;
use tilo_device_fabric::{DeviceFabric, DeviceId, FabricStamp, RoamingSetting};

/// How often the fabric is read when the settings here did not change: the
/// fabric is a whole snapshot of every device, so it is read a few times a
/// second rather than on every turn of the loop.
const SYNC_EVERY: Duration = Duration::from_millis(500);

/// The person's settings, kept in step with their other devices.
pub(crate) struct SettingsSync {
    /// When the fabric was last read.
    synced_at: Cell<Option<Instant>>,
    /// The settings document this device last offered, so an unchanged one
    /// is not offered again.
    seen: Cell<Option<settings_store::SettingsStamp>>,
    /// Whether the fabric this device's own records are in was read once:
    /// the first pass takes what the fabric still holds of them.
    seeded: Cell<bool>,
    /// What this device offered, by key.
    offered: RefCell<BTreeMap<String, RoamingSetting>>,
    /// The values this device took from the person's other devices, by key.
    applied: RefCell<BTreeMap<String, Value>>,
}

impl SettingsSync {
    pub(crate) fn new() -> Self {
        Self {
            synced_at: Cell::new(None),
            seen: Cell::new(None),
            seeded: Cell::new(false),
            offered: RefCell::new(BTreeMap::new()),
            applied: RefCell::new(BTreeMap::new()),
        }
    }

    /// Offer this device's settings to the person's other devices and take
    /// what they set. Returns whether the settings here changed.
    pub(crate) fn sync(&self, link: &dyn FabricLink) -> bool {
        let Some((local, _)) = link.local_device_fabric_identity() else {
            return false;
        };
        let stamp = settings_store::stamp();
        let changed =
            settings_store::document_changed(self.seen.get().is_some(), self.seen.get(), stamp);
        let due = self
            .synced_at
            .get()
            .is_none_or(|at| Instant::now().duration_since(at) >= SYNC_EVERY);
        if !due && !changed {
            return false;
        }
        self.synced_at.set(Some(Instant::now()));
        self.seen.set(stamp);
        let fabric = link.device_fabric();
        let document = settings_store::load();
        if !self.seeded.replace(true) {
            self.seed(&fabric, &local, &document);
        }
        self.offer(link, &fabric, &local, &document);
        self.take(&fabric, &local, &document)
    }

    /// Take what the fabric still holds of this device's own records, and of
    /// what the person set on their other devices and this one already
    /// shows: the first pass has no history of its own, and a value both
    /// devices agree on is not a change made here.
    fn seed(&self, fabric: &DeviceFabric, local: &DeviceId, document: &Map<String, Value>) {
        let offered = fabric
            .snapshots()
            .get(local)
            .map_or_else(Vec::new, |snapshot| snapshot.settings.clone());
        self.offered.replace(
            offered
                .into_iter()
                .map(|record| (record.key.clone(), record))
                .collect(),
        );
        self.applied.replace(
            fabric
                .merged_setting_records()
                .into_iter()
                .filter(|(_, record)| &record.stamp.device != local)
                .filter(|(key, record)| leaf_of(document, key) == Some(&record.value))
                .map(|(key, record)| (key, record.value))
                .collect(),
        );
    }

    /// Offer the person's settings this device holds, as this device's own
    /// records in its next snapshot.
    fn offer(
        &self,
        link: &dyn FabricLink,
        fabric: &DeviceFabric,
        local: &DeviceId,
        document: &Map<String, Value>,
    ) {
        let stamp = FabricStamp {
            counter: fabric.max_counter().saturating_add(1).max(1),
            device: local.clone(),
        };
        let applied = self.applied.borrow();
        let mut offered = self.offered.borrow_mut();
        let mut records = BTreeMap::new();
        for (key, value) in personal_leaves(document) {
            if serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len())
                > settings_store::placement::MAX_PERSONAL_VALUE_BYTES
            {
                tracing::warn!(%key, "a setting is too large for the person's devices");
                continue;
            }
            // What another device set and this one shows is theirs: offering
            // it again would make this device its author.
            if applied.get(&key) == Some(&value) {
                if let Some(old) = offered.get(&key) {
                    records.insert(key, old.clone());
                }
                continue;
            }
            let record = match offered.get(&key) {
                Some(old) if old.value == value => old.clone(),
                _ => RoamingSetting {
                    key: key.clone(),
                    value,
                    stamp: stamp.clone(),
                },
            };
            records.insert(key, record);
        }
        if records.values().eq(offered.values()) {
            return;
        }
        link.share_settings(records.values().cloned().collect());
        *offered = records;
    }

    /// Take what the person set on their other devices. Returns whether the
    /// settings here changed.
    fn take(&self, fabric: &DeviceFabric, local: &DeviceId, document: &Map<String, Value>) -> bool {
        let offered = self.offered.borrow();
        let theirs: BTreeMap<String, Value> = fabric
            .merged_setting_records()
            .into_iter()
            // A record this device wrote is a change made here, and one it
            // offered this pass is newer than anything the fabric held: a
            // value the person just set here is never undone by an older one
            // still in the fabric.
            .filter(|(_, record)| &record.stamp.device != local)
            .filter(|(key, record)| {
                offered
                    .get(key)
                    .is_none_or(|mine| !mine.stamp.is_newer_than(&record.stamp))
            })
            .map(|(key, record)| (key, record.value))
            .collect();
        let mut applied = self.applied.borrow_mut();
        if *applied == theirs {
            return false;
        }
        let mut sections = BTreeMap::<&str, Map<String, Value>>::new();
        for (key, value) in &theirs {
            let Some((section, leaf)) = key
                .split_once('.')
                .and_then(|(_, rest)| rest.split_once('.'))
            else {
                continue;
            };
            if leaf_of(document, key) == Some(value) {
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
        *applied = theirs;
        changed
    }
}

/// Every leaf of the sections the person holds, by its fabric key
/// (`tilo.<section>.<leaf>`). An object below a leaf travels as one value.
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

/// The value `document` holds at the fabric key `key`.
fn leaf_of<'a>(document: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let (section, leaf) = key.strip_prefix("tilo.")?.split_once('.')?;
    document.get(section)?.as_object()?.get(leaf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_persons_sections_are_offered() {
        let document: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "appearance": {"mode": "dark", "wallpaper": "dunes"},
            "app_personal": {"so.tilo.monitor": {"confirm_kill": true}},
            "app_device_local": {"so.tilo.monitor": {"device": 1}},
            "power": {"lock_minutes": 1},
        }))
        .unwrap();
        let offered = personal_leaves(&document);
        assert_eq!(
            offered.keys().collect::<Vec<_>>(),
            [
                "tilo.app_personal.so.tilo.monitor",
                "tilo.appearance.mode",
                "tilo.appearance.wallpaper",
            ],
            "this device's sections stay on it"
        );
        assert_eq!(
            leaf_of(&document, "tilo.app_personal.so.tilo.monitor"),
            offered.get("tilo.app_personal.so.tilo.monitor"),
            "an app's entry is one value, as it travels"
        );
        assert!(
            !offered.contains_key("tilo.power.lock_minutes"),
            "a switch of this machine's own does not travel"
        );
    }
}
