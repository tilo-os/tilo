//! Local journal Effects and the live entry catalogue.
//!
//! Shared filter/selection preferences are document actions on
//! [`crate::resource::Logs`]. Refresh, live-tail, and export are Effects with
//! no document delta. Entry rows expose a select View for agents; the widget
//! also commits that selection into the shared document.

use tilo_actions::{ActionRegistry, ActionSpec, OperationKind, Resource};

use crate::model::{Filter, LogEntry};

pub const LOGS_OPERATION_TYPE: &str = "logs.operation";
pub const JOURNAL_RESOURCE_ID: &str = "logs.journal";
const ENTRY_LIMIT: usize = 200;

/// Operations handled by the local Logs widget after resource admission.
///
/// Filter mutations are [`crate::resource::LogsAction`] document actions.
/// This enum covers Effects and the per-entry select View.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LogsOperation {
    SelectEntry {
        entry_id: String,
    },
    Refresh,
    SetLiveTail {
        enabled: bool,
    },
    /// Write only the filtered local cache through a host Save portal. The
    /// path never crosses the resource or collaboration boundary.
    ExportFiltered,
}

/// Decode one app-owned local operation.
///
/// # Errors
///
/// Returns an error when a payload claiming the Logs operation type is not
/// valid JSON for [`LogsOperation`].
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<LogsOperation>, String> {
    if type_id != LOGS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Logs document actions register through [`crate::resource::Logs`].
pub fn register(_registry: &mut ActionRegistry) {}

#[allow(
    clippy::expect_used,
    reason = "LogsOperation contains only serde-serializable primitives"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: &LogsOperation) -> ActionSpec {
    ActionSpec::operation(id, label, kind, LOGS_OPERATION_TYPE, value)
        .expect("Logs operation serializes")
}

/// Effect specs published on the journal resource (no document delta).
pub fn journal_effect_specs(live_tail: bool, allow_live: bool) -> Vec<ActionSpec> {
    let refresh = operation(
        "refresh",
        "Refresh journal",
        OperationKind::Effect,
        &LogsOperation::Refresh,
    )
    .retry_policy(tilo_actions::RetryPolicy::Idempotent);
    let refresh = if allow_live {
        refresh.requiring(tilo_actions::PermissionRequirement::Read)
    } else {
        refresh.unavailable("journal_access_disabled", "Live journal access is disabled")
    };

    let live = operation(
        "set-live-tail",
        if live_tail {
            "Stop live tail"
        } else {
            "Start live tail"
        },
        OperationKind::Effect,
        &LogsOperation::SetLiveTail {
            enabled: !live_tail,
        },
    )
    .retry_policy(tilo_actions::RetryPolicy::Idempotent);
    let live = if allow_live {
        live
    } else {
        live.unavailable("journal_access_disabled", "Live journal access is disabled")
    };

    // Export availability for filtered rows is enforced at perform time; the
    // static Resource door always offers the Effect when live access is on.
    let export = operation(
        "export-filtered-jsonl",
        "Export filtered logs",
        OperationKind::Effect,
        &LogsOperation::ExportFiltered,
    )
    .requiring(tilo_actions::PermissionRequirement::Write);
    let export = if allow_live {
        export
    } else {
        export.unavailable(
            "journal_access_disabled",
            "Live journal export is disabled in this session",
        )
    };

    vec![refresh, live, export]
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

/// Live journal entry rows (no `logs.journal` root).
pub fn live_entry_resources(entries: &[LogEntry], filter: &Filter) -> Vec<Resource> {
    entries
        .iter()
        .filter(|entry| filter.passes(entry))
        .take(ENTRY_LIMIT)
        .map(|entry| {
            let id = entry_resource_id(entry);
            Resource::new(
                id.clone(),
                "logs.entry",
                format!(
                    "{} from {} at {}",
                    entry.priority.long(),
                    entry.unit_short(),
                    entry.time_hms()
                ),
            )
            .with_action(operation(
                "select",
                "Show entry details",
                OperationKind::View,
                &LogsOperation::SelectEntry { entry_id: id },
            ))
        })
        .collect()
}

/// Standalone / `--shots` overlay: [`tilo_resource::publish`] root plus live
/// entry children, with effect labels reflecting the current live-tail state.
pub fn standalone_resources(
    entries: &[LogEntry],
    filter: &Filter,
    live_tail: bool,
    allow_live: bool,
    state: &crate::resource::LogsState,
) -> Vec<Resource> {
    let mut root = ui::tilo_resource::publish::<crate::resource::Logs>(
        JOURNAL_RESOURCE_ID,
        "System journal",
        state,
    );
    // Replace static effect specs with ones that reflect live-tail / allow_live.
    root.actions.retain(|spec| {
        !matches!(
            spec.id.as_str(),
            "refresh" | "set-live-tail" | "export-filtered-jsonl"
        )
    });
    for spec in journal_effect_specs(live_tail, allow_live) {
        root = root.with_action(spec);
    }
    // Dynamic export unavailable when the filtered set is empty.
    if allow_live
        && !entries.iter().any(|entry| filter.passes(entry))
        && let Some(export) = root
            .actions
            .iter_mut()
            .find(|action| action.id == "export-filtered-jsonl")
    {
        *export = operation(
            "export-filtered-jsonl",
            "Export filtered logs",
            OperationKind::Effect,
            &LogsOperation::ExportFiltered,
        )
        .requiring(tilo_actions::PermissionRequirement::Write)
        .unavailable(
            "no_matching_entries",
            "There are no filtered entries to export",
        );
    }
    root.children = live_entry_resources(entries, filter);
    vec![root]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use crate::resource::{Logs, LogsAction, LogsState};
    use tilo_actions::Action;
    use ui::tilo_resource::Resource as _;

    /// Entries publish a stable select View; shared filters live on the
    /// framework resource via [`tilo_resource::publish`].
    #[test]
    fn publish_owns_filters_and_entries_carry_select_views() {
        let mut doc = automerge::AutoCommit::new();
        LogsAction::SetSearch {
            value: "iroh".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        let state = Logs::read(&doc).unwrap();
        let published =
            ui::tilo_resource::publish::<Logs>(JOURNAL_RESOURCE_ID, "System journal", &state);
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-search"),
            "search declaration must be on the framework resource"
        );
        assert!(
            published.actions.iter().all(|action| action.id != "select"),
            "per-entry select stays on live entry children"
        );

        let entries = sample_entries();
        let entry_rows = live_entry_resources(&entries, &Filter::default());
        let id = entry_resource_id(&entries[0]);
        let entry = entry_rows.iter().find(|child| child.id.0 == id).unwrap();
        let select = entry
            .actions
            .iter()
            .find(|action| action.id == "select")
            .unwrap();
        assert_eq!(select.kind, OperationKind::View);
        let encoded = select.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(LogsOperation::SelectEntry { entry_id: id }),
        );
    }

    // Shared filter declarations plus Effects form the palette door; entry
    // select Views ride the standalone overlay's live children.
    ui::action_contract_tests! {
        seed: automerge::AutoCommit::new(),
        registry: {
            let mut registry = ActionRegistry::new();
            Logs::register(&mut registry);
            registry
        },
        resources: |doc| {
            let state = Logs::read(doc).unwrap_or_default();
            standalone_resources(
                &sample_entries(),
                &state.filter,
                false,
                true,
                &state,
            )
        },
        palette: ["Refresh journal", "Search journal", "Clear filters", "Show entry details"],
    }

    #[test]
    fn typed_search_declaration_builds_document_action() {
        let state = LogsState::default();
        let door = crate::resource::provide_resources(&state);
        let search = door[0]
            .actions
            .iter()
            .find(|action| action.id == "set-search")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert("query".into(), serde_json::json!("privacy"));
        let encoded = search.build_json(&args).unwrap();
        assert_eq!(encoded.type_id, Logs::ACTION_TYPE);
    }
}
