//! Replayable journal-view actions and the resource surface built from them.
//!
//! Journal records are external, read-only data: refreshing them is a local
//! effect, never a collaborative mutation. Filters and the selected record
//! are small shared user decisions, so they travel as typed document actions
//! and create ordinary undo boundaries. Live tail is intentionally a local
//! effect: it controls a process-local journal poller and must not surprise a
//! collaborator by starting work on their device.

use automerge::{AutoCommit, ROOT};
use qos_actions::{
    Action, ActionArgs, ActionBinding, ActionError, ActionParam, ActionRegistry, ActionSpec,
    EncodedAction, ExecutionLocus, OperationKind, ParamType, Resource,
};

use crate::doc::LogsDoc;
use crate::model::{Filter, LogEntry};

pub const LOGS_ACTION_TYPE: &str = "logs.action";
pub const LOGS_OPERATION_TYPE: &str = "logs.operation";
pub const JOURNAL_RESOURCE_ID: &str = "logs.journal";
const ENTRY_LIMIT: usize = 200;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LogsAction {
    SetPriority { value: i64 },
    SetUnit { value: String },
    SetBoot { value: String },
    SetSearch { value: String },
    ClearFilters,
    SelectEntry { entry_id: String },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LogsOperation {
    Refresh,
    SetLiveTail {
        enabled: bool,
    },
    /// Write only the currently filtered local cache to a host-portal
    /// destination. The path never crosses the action boundary.
    ExportFiltered,
}

#[allow(
    clippy::expect_used,
    reason = "the unit-only Logs operation is statically serializable"
)]
fn export_filtered_spec() -> ActionSpec {
    ActionSpec::operation(
        "export-filtered-jsonl",
        "Export filtered logs",
        OperationKind::Effect,
        LOGS_OPERATION_TYPE,
        &LogsOperation::ExportFiltered,
    )
    .expect("unit Logs operation serializes")
    // An exported log stream may expose secrets even though it does not alter
    // the journal itself. The portal Save selection is the user's explicit
    // authorization; this ordinary write is not a destructive operation.
    .requiring(qos_actions::PermissionRequirement::Write)
}

/// Decode an app-owned local journal operation.
///
/// # Errors
///
/// Returns an error when a Logs operation payload is malformed JSON.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<LogsOperation>, String> {
    if type_id != LOGS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

impl Action for LogsAction {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        match self {
            Self::SetPriority { value } => LogsDoc::set_min_priority(doc, &ROOT, *value),
            Self::SetUnit { value } => LogsDoc::set_unit(doc, &ROOT, value),
            Self::SetBoot { value } => LogsDoc::set_boot(doc, &ROOT, value),
            Self::SetSearch { value } => LogsDoc::search_set(doc, &ROOT, value),
            Self::ClearFilters => LogsDoc::reset_filters(doc, &ROOT),
            Self::SelectEntry { entry_id } => LogsDoc::set_selected_entry(doc, &ROOT, entry_id),
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<LogsAction>(LOGS_ACTION_TYPE);
}

#[allow(
    clippy::expect_used,
    reason = "LogsAction contains only serializable primitives"
)]
/// Encode a typed Logs action for resource dispatch.
///
/// # Panics
///
/// Never in practice: every `LogsAction` field is serializable.
pub fn encode(action: &LogsAction) -> EncodedAction {
    EncodedAction::encode(LOGS_ACTION_TYPE, action).expect("LogsAction serializes")
}

#[allow(
    clippy::expect_used,
    reason = "LogsAction contains only serializable primitives"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "ActionSpec owns an encoded snapshot; taking the action by value makes that boundary explicit."
)]
fn immediate(id: &str, label: &str, action: LogsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, LOGS_ACTION_TYPE, &action)
        .expect("LogsAction serializes")
        .reversible()
}

fn set_search_spec() -> ActionSpec {
    ActionSpec::typed(
        "set-search",
        "Search journal",
        "text to find in messages, units, or identifiers",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "query",
            ParamType::Text,
            "Search query",
        )],
        |args: &ActionArgs| {
            encode(&LogsAction::SetSearch {
                value: args.text("query").unwrap_or_default().to_owned(),
            })
        },
    )
    .reversible()
}

fn set_priority_spec() -> ActionSpec {
    ActionSpec::typed(
        "set-severity",
        "Set severity threshold",
        "0 through 7 (0 emergency only; 7 all entries)",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "priority",
                ParamType::Integer,
                "Severity threshold from 0 to 7",
            )
            .with_schema(serde_json::json!({ "minimum": 0, "maximum": 7 })),
        ],
        |args: &ActionArgs| {
            encode(&LogsAction::SetPriority {
                value: args.integer("priority").unwrap_or(7),
            })
        },
    )
    .reversible()
}

fn set_text_filter_spec(id: &str, label: &str, parameter: &'static str, boot: bool) -> ActionSpec {
    ActionSpec::typed(
        id,
        label,
        "exact value; leave empty to show all",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::optional_owned(
            parameter,
            ParamType::Text,
            "Filter value",
        )],
        move |args: &ActionArgs| {
            let value = args.text(parameter).unwrap_or_default().to_owned();
            encode(&if boot {
                LogsAction::SetBoot { value }
            } else {
                LogsAction::SetUnit { value }
            })
        },
    )
    .reversible()
}

/// Stable across paths, display order, and journal cache refreshes.
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

/// Build the one resource tree used by widgets, search, and remote agents.
/// Entry materialization is deliberately capped; the current filters are
/// themselves addressable actions, so callers can narrow a large journal
/// before enumerating more of it.
pub fn resources(
    entries: &[LogEntry],
    filter: &Filter,
    live_tail: bool,
    allow_live: bool,
) -> Vec<Resource> {
    let filters = Resource::new("logs.filters", "logs.filters", "Journal filters")
        .with_action(set_search_spec())
        .with_action(set_priority_spec())
        .with_action(set_text_filter_spec(
            "set-unit",
            "Filter by unit",
            "unit",
            false,
        ))
        .with_action(set_text_filter_spec(
            "set-boot",
            "Filter by boot",
            "boot",
            true,
        ))
        .with_action(immediate(
            "clear-filters",
            "Clear filters",
            LogsAction::ClearFilters,
        ));

    let mut root = Resource::new(JOURNAL_RESOURCE_ID, "logs.journal", "System journal");

    let refresh = ActionSpec::operation(
        "refresh",
        "Refresh journal",
        OperationKind::Effect,
        LOGS_OPERATION_TYPE,
        &LogsOperation::Refresh,
    )
    .map(|spec| spec.retry_policy(qos_actions::RetryPolicy::Idempotent));
    if let Ok(spec) = refresh {
        root = root.with_action(if allow_live {
            spec.requiring(qos_actions::PermissionRequirement::Read)
        } else {
            spec.unavailable("journal_access_disabled", "Live journal access is disabled")
        });
    }

    if let Ok(spec) = ActionSpec::operation(
        "set-live-tail",
        if live_tail {
            "Stop live tail"
        } else {
            "Start live tail"
        },
        OperationKind::Effect,
        LOGS_OPERATION_TYPE,
        &LogsOperation::SetLiveTail {
            enabled: !live_tail,
        },
    ) {
        let spec = spec.retry_policy(qos_actions::RetryPolicy::Idempotent);
        root = root.with_action(if allow_live {
            spec
        } else {
            spec.unavailable("journal_access_disabled", "Live journal access is disabled")
        });
    }
    let export = export_filtered_spec();
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
    root = root.with_child(filters);

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
            .with_action(immediate(
                "select",
                "Show entry details",
                LogsAction::SelectEntry { entry_id: id },
            )),
        );
    }
    vec![root]
}

/// Bind the shared search control to the one canonical filters action.
///
/// # Panics
///
/// Panics only if this module's static resource definition stops exporting
/// `set-search`, which is a programmer error covered by the resource tests.
#[allow(
    clippy::expect_used,
    reason = "the static resource always exports set-search"
)]
pub fn search_binding() -> ActionBinding<String> {
    let resource = resources(&[], &Filter::default(), false, false)
        .into_iter()
        .next()
        .expect("journal resource exists");
    let filters = resource
        .children
        .iter()
        .find(|child| child.id.0 == "logs.filters")
        .expect("filters resource exists");
    let spec = filters
        .actions
        .iter()
        .find(|spec| spec.id == "set-search")
        .expect("set-search is exported");
    ActionBinding::new(filters, spec, |query| {
        serde_json::Map::from_iter([(String::from("query"), serde_json::Value::String(query))])
    })
    .expect("static search binding validates")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::sample_entries;
    use qos_actions::{ActionInput, ActionRegistry};

    #[test]
    fn entry_ids_and_selection_actions_are_stable_and_replayable() {
        let entries = sample_entries();
        let id = entry_resource_id(&entries[0]);
        assert_eq!(id, entry_resource_id(&entries[0]));
        let resource = resources(&entries, &Filter::default(), false, false);
        let entry = resource[0]
            .children
            .iter()
            .find(|child| child.id.0 == id)
            .unwrap();
        let select = entry
            .actions
            .iter()
            .find(|action| action.id == "select")
            .unwrap();
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let mut doc = AutoCommit::new();
        let ActionInput::Immediate(encoded) = &select.input else {
            panic!("select must be immediate");
        };
        registry.decode(encoded).unwrap().apply(&mut doc).unwrap();
        assert_eq!(
            LogsDoc::selected_entry(&doc, &ROOT).as_deref(),
            Some(id.as_str())
        );
    }

    #[test]
    fn journal_resources_expose_actions_and_never_offer_disabled_refresh() {
        let root = resources(&sample_entries(), &Filter::default(), false, false).remove(0);
        assert!(!root.actions.iter().any(|action| action.id == "set-search"));
        let filters = root
            .children
            .iter()
            .find(|child| child.kind == "logs.filters")
            .expect("filters resource is present");
        assert!(
            filters
                .actions
                .iter()
                .any(|action| action.id == "set-search")
        );
        let refresh = root
            .actions
            .iter()
            .find(|action| action.id == "refresh")
            .unwrap();
        assert!(matches!(
            refresh.contract.availability,
            qos_actions::Availability::Unavailable { .. }
        ));
        let live_tail = root
            .actions
            .iter()
            .find(|action| action.id == "set-live-tail")
            .unwrap();
        assert!(matches!(
            live_tail.contract.availability,
            qos_actions::Availability::Unavailable { .. }
        ));
        let export = root
            .actions
            .iter()
            .find(|action| action.id == "export-filtered-jsonl")
            .expect("filtered export is represented in the shared action surface");
        assert_eq!(export.kind, OperationKind::Effect);
        assert_eq!(
            export.contract.permission,
            qos_actions::PermissionRequirement::Write
        );
        assert_eq!(export.contract.risk, qos_actions::RiskLevel::Low);
        assert_eq!(
            export.contract.confirmation,
            qos_actions::ConfirmationPolicy::None
        );
        assert_eq!(export.contract.retry, qos_actions::RetryPolicy::AtMostOnce);
    }

    #[test]
    fn export_is_a_pathless_portal_operation_without_publishing_log_content() {
        let root = resources(&sample_entries(), &Filter::default(), false, true).remove(0);
        let export = root
            .actions
            .iter()
            .find(|action| action.id == "export-filtered-jsonl")
            .unwrap();
        let encoded = export.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(LogsOperation::ExportFiltered)
        );
        assert!(!root.label.contains("surface lost"));
    }
}
