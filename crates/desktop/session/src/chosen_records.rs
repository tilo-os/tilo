//! What the session keeps of the files people chose for apps outside any workspace, in the
//! device's own records ([`tilo_session_client::device_records`]).
//!
//! The table itself ([`tilo_dialogs::chosen`]) is the one the chooser and the portal share; this
//! is where it lives between sessions. One record holds every choice, as the same entries the
//! table carries, written through the device's `settings#set` action and read with its one record
//! read: the person's choices are sealed to them and reach their other devices the way their
//! device's other records do. The record is followed while the session runs, so a choice Settings
//! or an agent takes back elsewhere leaves the table as it lands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_dialogs::chosen::{Kept, RECORD, Store};
use tilo_lock::MutexRecover as _;
use tilo_session_client::device_records::{DeviceRecord, DeviceRecords};

/// The device's records, as the table of chosen files reads and writes them.
pub(crate) struct DeviceStore {
    records: DeviceRecords,
    /// The record as followed, once it is: what this session keeps, and what it hears of changes
    /// written elsewhere through.
    following: Mutex<Option<DeviceRecord>>,
    /// Whether following was tried, so a read that cannot be followed is not asked for again.
    followed: AtomicBool,
    /// What wakes the session's loop when the record changes elsewhere.
    wake: tilo_service::Wake,
}

impl DeviceStore {
    pub(crate) fn new(records: DeviceRecords, wake: tilo_service::Wake) -> Self {
        Self {
            records,
            following: Mutex::default(),
            followed: AtomicBool::default(),
            wake,
        }
    }
}

/// The choices `value` holds, as the record keeps them.
fn kept_of(value: Option<Value>) -> Vec<Kept> {
    match value {
        Some(Value::Array(entries)) => entries
            .into_iter()
            .filter_map(|entry| {
                tilo_failure::ignored!(
                    "a kept choice that does not read is skipped, and the rest still open",
                    serde_json::from_value(entry).ok()
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl Store for DeviceStore {
    fn load(&self) -> Vec<Kept> {
        match self.records.record(RECORD).read() {
            Ok(value) => kept_of(value),
            Err(error) => {
                tracing::warn!(%error, "the files people chose earlier are not read");
                Vec::new()
            }
        }
    }

    fn keep(&self, kept: Vec<Kept>) {
        let value = serde_json::to_value(kept).unwrap_or_else(|_| Value::Array(Vec::new()));
        match self.following.lock_recover().as_ref() {
            // Through the followed record, so this session's own write is no change from
            // elsewhere to read back.
            Some(record) => record.set(value),
            None => self.records.record(RECORD).set(value),
        }
    }

    fn follow(&self, changed: Arc<dyn Fn(Vec<Kept>) + Send + Sync>) -> bool {
        if self.followed.swap(true, Ordering::AcqRel) {
            return self.following.lock_recover().is_some();
        }
        let wake = Arc::clone(&self.wake);
        let handed = self.records.record(RECORD).follow(move |value| {
            changed(kept_of(value));
            wake();
        });
        match handed {
            Ok(record) => {
                *self.following.lock_recover() = Some(record);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "the files people chose are not followed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wake() -> tilo_service::Wake {
        Arc::new(|| ())
    }

    #[test]
    fn a_choice_written_is_the_choice_read_back() {
        let records = DeviceRecords::held();
        let store = DeviceStore::new(records, wake());
        let held = vec![Kept {
            workspace: "5ba25f73-87c5-447f-a5a6-880dc4249a81".to_owned(),
            app: "so.tilo.notes".to_owned(),
            kind: "read".to_owned(),
            host: std::path::PathBuf::from("/home/ada/plan.txt"),
        }];
        store.keep(held.clone());
        assert_eq!(store.load(), held);
    }
}
