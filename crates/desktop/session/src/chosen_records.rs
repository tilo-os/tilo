//! What the session keeps of the files people chose for apps outside any workspace, in the
//! device's own records ([`tilo_session_client::device_records`]).
//!
//! The table itself ([`tilo_dialogs::chosen`]) is the one the chooser and the portal share; this
//! is where it lives between sessions. One record holds every choice, as the same entries the
//! table carries, written through the device's `settings#set` action and read with its one record
//! read: the person's choices are sealed to them and reach their other devices the way their
//! device's other records do.

use serde_json::Value;
use tilo_dialogs::chosen::{Kept, RECORD, Store};
use tilo_session_client::device_records::DeviceRecords;

/// The device's records, as the table of chosen files reads and writes them.
pub(crate) struct DeviceStore {
    records: DeviceRecords,
}

impl DeviceStore {
    pub(crate) const fn new(records: DeviceRecords) -> Self {
        Self { records }
    }
}

impl Store for DeviceStore {
    fn load(&self) -> Vec<Kept> {
        match self.records.record(RECORD).read() {
            Ok(Some(Value::Array(entries))) => entries
                .into_iter()
                .filter_map(|entry| {
                    tilo_failure::ignored!(
                        "a kept choice that does not read is skipped, and the rest still open",
                        serde_json::from_value(entry).ok()
                    )
                })
                .collect(),
            Ok(_) => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "the files people chose earlier are not read");
                Vec::new()
            }
        }
    }

    fn keep(&self, kept: Vec<Kept>) {
        let value = serde_json::to_value(kept).unwrap_or_else(|_| Value::Array(Vec::new()));
        self.records.record(RECORD).set(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_written_is_the_choice_read_back() {
        let records = DeviceRecords::held();
        let store = DeviceStore::new(records);
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
