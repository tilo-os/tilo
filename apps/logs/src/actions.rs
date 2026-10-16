//! What Logs does beyond its filter: the journal effects, and the entries
//! it publishes as live rows.
//!
//! The filter is the [`crate::resource::Logs`] document. Refresh, live tail,
//! export and showing an entry change no document, so they are effects
//! answered by this device's viewer. Entries are this device's journal,
//! published with stable ids and metadata only, never their payloads.

use ui::prelude::*;
use ui::tilo_actions::ResourcePresentation;

use crate::model::{Filter, LogEntry};

/// The most entries published as live rows; the rest resolve by id.
const ENTRY_LIMIT: usize = 200;

/// The journal effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "logs.effects", device_only)]
pub enum JournalEffect {
    /// Refresh journal.
    #[effect(job, kind = "read")]
    Refresh,
    /// Start live tail.
    #[effect(kind = "read")]
    SetLiveTail {
        #[param("Follow new entries; omit to toggle")]
        enabled: Option<bool>,
    },
    /// Export filtered logs.
    ///
    /// Writes the entries the filter shows to a JSON Lines file the person
    /// picks.
    #[effect(job)]
    ExportFilteredJsonl,
    /// Show entry details.
    #[effect(kind = "read")]
    SelectEntry {
        #[param("Stable logs.entry.* id")]
        entry_id: String,
    },
}

/// Stable across ordering and cache refreshes. It uses journal fields rather
/// than a display index, so every local cache can address the same record.
pub fn entry_resource_id(entry: &LogEntry) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [
        entry.boot_id.as_bytes(),
        &entry.realtime_us.to_le_bytes(),
        entry.unit.as_bytes(),
        entry.identifier.as_bytes(),
        entry.message.as_bytes(),
    ] {
        hasher.update(part);
        hasher.update(&[0]);
    }
    format!("logs.entry.{}", hasher.finalize().to_hex())
}

/// One entry as a live row, offering to show its details.
pub fn entry_row(entry: &LogEntry) -> LiveRow {
    let id = entry_resource_id(entry);
    LiveRow::new(id.clone(), "logs.entry", entry.unit_short())
        .presentation(ResourcePresentation {
            detail: Some(entry.time_hms()),
            badge: Some(entry.priority.long().to_string()),
            ..Default::default()
        })
        .operation(JournalEffect::SelectEntry { entry_id: id }.bound())
}

/// The first entries `filter` shows, as live rows.
pub fn entry_rows(entries: &[LogEntry], filter: &Filter) -> Vec<LiveRow> {
    entries
        .iter()
        .filter(|entry| filter.passes(entry))
        .take(ENTRY_LIMIT)
        .map(entry_row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use ui::tilo_actions::OperationKind;

    #[test]
    fn entry_rows_offer_to_show_themselves() {
        let entries = sample_entries();
        let rows = entry_rows(&entries, &Filter::default());
        let id = entry_resource_id(&entries[0]);
        let row = rows
            .into_iter()
            .find(|row| row.id() == id)
            .unwrap()
            .into_resource();
        let select = row
            .actions
            .iter()
            .find(|action| action.id == "select-entry")
            .unwrap();
        assert_eq!(select.kind, OperationKind::Effect);
        let encoded = select.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(
            JournalEffect::from_payload(&encoded.payload).unwrap(),
            JournalEffect::SelectEntry { entry_id: id }
        );
    }
}
