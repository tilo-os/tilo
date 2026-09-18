//! Local journal-view operations and the searchable Logs resource surface.
//!
//! The journal is an observation of this machine. Querying, selecting, and
//! following it are therefore local view operations, not collaborative
//! document mutations. Resources still expose every navigable entry and every
//! supported operation through deterministic addresses for palettes and
//! agents; their payloads contain only small local view values or stable entry
//! identities, never journal content.

use tilo_actions::{
    ActionArgs, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind, ParamType,
    Resource,
};

use crate::model::{Filter, LogEntry};

pub const LOGS_OPERATION_TYPE: &str = "logs.operation";
pub const JOURNAL_RESOURCE_ID: &str = "logs.journal";
const ENTRY_LIMIT: usize = 200;

/// Operations handled by the local Logs widget after resource admission.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LogsOperation {
    SetPriority {
        value: i64,
    },
    SetUnit {
        value: String,
    },
    SetBoot {
        value: String,
    },
    SetSearch {
        value: String,
    },
    ClearFilters,
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

/// Logs has no durable document actions. Retained for the launch hook shared
/// by Tilo apps, so upgraded clients can open old Logs documents safely.
pub fn register(_registry: &mut ActionRegistry) {}

#[allow(
    clippy::expect_used,
    reason = "LogsOperation contains only serde-serializable primitives"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: &LogsOperation) -> ActionSpec {
    ActionSpec::operation(id, label, kind, LOGS_OPERATION_TYPE, value)
        .expect("Logs operation serializes")
}

#[allow(
    clippy::expect_used,
    reason = "LogsOperation contains only serde-serializable primitives"
)]
fn typed_operation(
    id: &str,
    label: &str,
    description: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> LogsOperation + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        description,
        OperationKind::View,
        LOGS_OPERATION_TYPE,
        params,
        move |args| {
            EncodedAction::encode_operation(LOGS_OPERATION_TYPE, &build(args))
                .expect("Logs operation serializes")
        },
    )
}

fn filter_resource() -> Resource {
    Resource::new("logs.filters", "logs.filters", "Local journal filters")
        .with_action(typed_operation(
            "set-search",
            "Search journal",
            "text to find in messages, units, or identifiers",
            vec![ActionParam::required(
                "query",
                ParamType::Text,
                "Search query",
            )],
            |args| LogsOperation::SetSearch {
                value: args.text("query").unwrap_or_default().to_owned(),
            },
        ))
        .with_action(typed_operation(
            "set-severity",
            "Set severity threshold",
            "0 through 7 (0 emergency only; 7 all entries)",
            vec![
                ActionParam::required(
                    "priority",
                    ParamType::Integer,
                    "Severity threshold from 0 to 7",
                )
                .with_schema(serde_json::json!({ "minimum": 0, "maximum": 7 })),
            ],
            |args| LogsOperation::SetPriority {
                value: args.integer("priority").unwrap_or(7),
            },
        ))
        .with_action(typed_operation(
            "set-unit",
            "Filter by unit",
            "exact value; leave empty to show all",
            vec![ActionParam::optional(
                "unit",
                ParamType::Text,
                "Filter value",
            )],
            |args| LogsOperation::SetUnit {
                value: args.text("unit").unwrap_or_default().to_owned(),
            },
        ))
        .with_action(typed_operation(
            "set-boot",
            "Filter by boot",
            "exact value; leave empty to show all",
            vec![ActionParam::optional(
                "boot",
                ParamType::Text,
                "Filter value",
            )],
            |args| LogsOperation::SetBoot {
                value: args.text("boot").unwrap_or_default().to_owned(),
            },
        ))
        .with_action(operation(
            "clear-filters",
            "Clear filters",
            OperationKind::View,
            &LogsOperation::ClearFilters,
        ))
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

/// Build the resource tree used by widgets, search, and remote agents.
/// Materialization is capped so a large local journal never floods discovery.
pub fn resources(
    entries: &[LogEntry],
    filter: &Filter,
    live_tail: bool,
    allow_live: bool,
) -> Vec<Resource> {
    let mut root = Resource::new(JOURNAL_RESOURCE_ID, "logs.journal", "System journal");
    let refresh = operation(
        "refresh",
        "Refresh journal",
        OperationKind::Effect,
        &LogsOperation::Refresh,
    )
    .retry_policy(tilo_actions::RetryPolicy::Idempotent);
    root = root.with_action(if allow_live {
        refresh.requiring(tilo_actions::PermissionRequirement::Read)
    } else {
        refresh.unavailable("journal_access_disabled", "Live journal access is disabled")
    });
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
    root = root.with_action(if allow_live {
        live
    } else {
        live.unavailable("journal_access_disabled", "Live journal access is disabled")
    });
    let export = operation(
        "export-filtered-jsonl",
        "Export filtered logs",
        OperationKind::Effect,
        &LogsOperation::ExportFiltered,
    )
    .requiring(tilo_actions::PermissionRequirement::Write);
    root = root.with_action(if !allow_live {
        export.unavailable(
            "journal_access_disabled",
            "Live journal export is disabled in this session",
        )
    } else if entries.iter().any(|entry| filter.passes(entry)) {
        export
    } else {
        export.unavailable(
            "no_matching_entries",
            "There are no filtered entries to export",
        )
    });
    root = root.with_child(filter_resource());
    for entry in entries
        .iter()
        .filter(|entry| filter.passes(entry))
        .take(ENTRY_LIMIT)
    {
        let id = entry_resource_id(entry);
        root = root.with_child(
            Resource::new(
                id.clone(),
                "logs.entry",
                format!(
                    "{} · {} · {}",
                    entry.time_hms(),
                    entry.priority.long(),
                    entry.unit_short()
                ),
            )
            .with_action(operation(
                "select",
                "Show entry details",
                OperationKind::View,
                &LogsOperation::SelectEntry { entry_id: id },
            )),
        );
    }
    vec![root]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;

    #[test]
    fn entries_have_stable_local_navigation_resources() {
        let entries = sample_entries();
        let id = entry_resource_id(&entries[0]);
        let root = resources(&entries, &Filter::default(), false, false).remove(0);
        let entry = root.children.iter().find(|child| child.id.0 == id).unwrap();
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

    #[test]
    fn filters_are_local_view_operations_not_document_actions() {
        let root = resources(&sample_entries(), &Filter::default(), false, false).remove(0);
        let filters = root
            .children
            .iter()
            .find(|child| child.id.0 == "logs.filters")
            .unwrap();
        assert!(
            filters
                .actions
                .iter()
                .all(|action| action.kind == OperationKind::View)
        );
        assert!(
            filters
                .actions
                .iter()
                .any(|action| action.id == "set-search")
        );
    }
}
