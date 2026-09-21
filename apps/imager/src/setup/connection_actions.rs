//! Path-free semantic resources and operations for Connected folder management.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionSpec, EncodedAction, OperationKind, Resource, ResourcePresentation, ResultSpec,
};

use super::strings;

pub const CONNECTION_RESOURCE_ID: &str = "imager.connections";
pub const CONNECTION_OPERATION_TYPE: &str = "imager.connection.operation";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ConnectionOperation {
    OpenManagement { generation: u64 },
    BackToImports { generation: u64 },
    Observe { generation: u64 },
    ReloadConnections { generation: u64 },
    FirstConnections { generation: u64 },
    NextConnections { generation: u64 },
    OpenSettings { generation: u64, alias: u64 },
    SyncNow { generation: u64, alias: u64 },
    Pause { generation: u64, alias: u64 },
    Disconnect { generation: u64, alias: u64 },
    AcceptInitial { generation: u64, alias: u64 },
    Restore { generation: u64, alias: u64 },
    Replace { generation: u64, alias: u64 },
}

impl ConnectionOperation {
    #[must_use]
    pub const fn generation(self) -> u64 {
        match self {
            Self::OpenManagement { generation }
            | Self::BackToImports { generation }
            | Self::Observe { generation }
            | Self::ReloadConnections { generation }
            | Self::FirstConnections { generation }
            | Self::NextConnections { generation }
            | Self::OpenSettings { generation, .. }
            | Self::SyncNow { generation, .. }
            | Self::Pause { generation, .. }
            | Self::Disconnect { generation, .. }
            | Self::AcceptInitial { generation, .. }
            | Self::Restore { generation, .. }
            | Self::Replace { generation, .. } => generation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementPhase {
    Loading,
    Unavailable,
    Empty,
    Catalog,
    Settings,
}

impl ManagementPhase {
    const fn detail_key(self) -> &'static str {
        match self {
            Self::Loading => "imager-connected-detail-phase-loading",
            Self::Unavailable => "imager-connected-detail-phase-unavailable",
            Self::Empty => "imager-connected-detail-phase-empty",
            Self::Catalog => "imager-connected-detail-phase-catalog",
            Self::Settings => "imager-connected-detail-phase-settings",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectionProjection {
    pub alias: u64,
    pub number: usize,
    pub revision: u64,
    pub lifecycle: String,
    pub has_baseline: bool,
    pub selected: bool,
    pub open_settings_available: bool,
    pub open_settings_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndpointIssueProjection {
    pub side: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectedProjection {
    pub alias: u64,
    pub phase: String,
    pub verified_steps: u32,
    pub pending_operation: bool,
    pub endpoint_issue: Option<EndpointIssueProjection>,
    pub awaiting_entries: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManagementProjection {
    pub generation: u64,
    pub phase: ManagementPhase,
    pub status_code: String,
    pub status_is_error: bool,
    pub rows: Vec<ConnectionProjection>,
    pub selected: Option<SelectedProjection>,
    pub reload_available: bool,
    pub next_available: bool,
    pub first_available: bool,
    pub navigation_unavailable_reason: Option<String>,
}

impl ManagementProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

#[must_use]
pub fn encode(operation: &ConnectionOperation) -> EncodedAction {
    EncodedAction::encode_operation(CONNECTION_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: CONNECTION_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

/// Decode one app-owned local operation without admitting it.
///
/// # Errors
/// Returns a localized error for malformed or extra fields.
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<ConnectionOperation>, String> {
    if type_id != CONNECTION_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload).map(Some).map_err(|error| {
        strings::t_args(
            "imager-connected-error-invalid-operation",
            &[("error", error.to_string().into())],
        )
    })
}

#[must_use]
pub const fn expected_kind(operation: &ConnectionOperation) -> OperationKind {
    match operation {
        ConnectionOperation::SyncNow { .. }
        | ConnectionOperation::Pause { .. }
        | ConnectionOperation::Disconnect { .. }
        | ConnectionOperation::AcceptInitial { .. }
        | ConnectionOperation::Restore { .. }
        | ConnectionOperation::Replace { .. } => OperationKind::Effect,
        ConnectionOperation::OpenManagement { .. }
        | ConnectionOperation::BackToImports { .. }
        | ConnectionOperation::Observe { .. }
        | ConnectionOperation::ReloadConnections { .. }
        | ConnectionOperation::FirstConnections { .. }
        | ConnectionOperation::NextConnections { .. }
        | ConnectionOperation::OpenSettings { .. } => OperationKind::View,
    }
}

/// # Errors
/// Refuses a command invoked through a different semantic door.
pub fn validate_kind(operation: &ConnectionOperation, kind: OperationKind) -> Result<(), String> {
    if expected_kind(operation) == kind {
        Ok(())
    } else {
        Err(strings::t("imager-connected-error-operation-kind"))
    }
}

fn result_spec() -> ResultSpec {
    ResultSpec::Json {
        schema: serde_json::json!({ "type": "object" }),
    }
}

fn action(
    id: &str,
    label_key: &str,
    operation: ConnectionOperation,
    available: bool,
    unavailable_code: &str,
    unavailable_key: &str,
) -> ActionSpec {
    let mut action = ActionSpec::operation(
        id,
        strings::t(label_key),
        expected_kind(&operation),
        CONNECTION_OPERATION_TYPE,
        &operation,
    )
    .unwrap_or_else(|_| {
        ActionSpec::typed_operation(
            id,
            strings::t(label_key),
            "",
            expected_kind(&operation),
            CONNECTION_OPERATION_TYPE,
            Vec::new(),
            move |_| encode(&operation),
        )
    })
    .returns(result_spec());
    if !available {
        action = action.unavailable(unavailable_code, strings::t(unavailable_key));
    }
    action
}

fn unavailable(
    reason: Option<&str>,
    fallback_code: &'static str,
    fallback_key: &'static str,
) -> (&'static str, &'static str) {
    match reason {
        Some("request_pending") => ("request_pending", "imager-connected-error-request-pending"),
        Some("open_pending") => ("open_pending", "imager-connected-error-open-pending"),
        Some("switch_busy") => ("switch_busy", "imager-connected-error-switch-busy"),
        Some("service_unavailable") => (
            "service_unavailable",
            "imager-connected-error-service-unavailable",
        ),
        _ => (fallback_code, fallback_key),
    }
}

#[must_use]
pub fn entry_resource(generation: u64) -> Resource {
    Resource::new(
        format!("{CONNECTION_RESOURCE_ID}.entry"),
        "imager.connections.entry",
        strings::t("imager-connected-resource-entry"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t("imager-connected-detail-entry")),
        badge: Some(strings::t("imager-connected-badge-entry")),
    })
    .with_action(action(
        "open-management",
        "imager-connected-action-open-management",
        ConnectionOperation::OpenManagement { generation },
        true,
        "stale_route",
        "imager-connected-error-stale-route",
    ))
}

#[must_use]
pub(crate) fn row_detail(row: &ConnectionProjection) -> String {
    let key = match (row.lifecycle.as_str(), row.has_baseline) {
        ("active", false) => "imager-connected-row-active-initial",
        ("active", true) => "imager-connected-row-active-compared",
        ("disconnecting", _) => "imager-connected-row-disconnecting",
        ("disconnected", _) => "imager-connected-row-disconnected",
        _ => "imager-connected-row-saved",
    };
    strings::t(key)
}

fn row_resource(state: &ManagementProjection, row: &ConnectionProjection) -> Resource {
    let generation = state.generation;
    let alias = row.alias;
    let mut resource = Resource::new(
        format!("{CONNECTION_RESOURCE_ID}.row.{generation}.{alias}"),
        "imager.connection",
        strings::t_args(
            "imager-connected-resource-row",
            &[("number", row.number.into())],
        ),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(row_detail(row)),
        badge: Some(strings::t(if row.selected {
            "imager-connected-badge-selected-row"
        } else {
            "imager-connected-badge-row"
        })),
    })
    .with_action(action(
        "open-settings",
        "imager-connected-action-open-settings",
        ConnectionOperation::OpenSettings { generation, alias },
        row.open_settings_available,
        unavailable(
            row.open_settings_unavailable_reason.as_deref(),
            "stale_connection",
            "imager-connected-error-stale-connection",
        )
        .0,
        unavailable(
            row.open_settings_unavailable_reason.as_deref(),
            "stale_connection",
            "imager-connected-error-stale-connection",
        )
        .1,
    ));
    if row.selected {
        for (id, key, operation) in [
            (
                "sync-now",
                "imager-connected-action-sync-now",
                ConnectionOperation::SyncNow { generation, alias },
            ),
            (
                "pause",
                "imager-connected-action-pause",
                ConnectionOperation::Pause { generation, alias },
            ),
            (
                "disconnect",
                "imager-connected-action-disconnect",
                ConnectionOperation::Disconnect { generation, alias },
            ),
            (
                "accept-initial",
                "imager-connected-action-accept-initial",
                ConnectionOperation::AcceptInitial { generation, alias },
            ),
            (
                "restore",
                "imager-connected-action-restore",
                ConnectionOperation::Restore { generation, alias },
            ),
            (
                "replace",
                "imager-connected-action-replace",
                ConnectionOperation::Replace { generation, alias },
            ),
        ] {
            resource = resource.with_action(action(
                id,
                key,
                operation,
                false,
                "native_capability_closed",
                "imager-connected-error-native-capability-closed",
            ));
        }
    }
    resource
}

/// Build the bounded path-free resource projection for the current management route.
#[must_use]
pub fn resource(state: &ManagementProjection) -> Resource {
    let generation = state.generation;
    let reload_unavailable = unavailable(
        state.navigation_unavailable_reason.as_deref(),
        "request_pending",
        "imager-connected-error-request-pending",
    );
    let first_unavailable = unavailable(
        state.navigation_unavailable_reason.as_deref(),
        "first_unavailable",
        "imager-connected-error-first-unavailable",
    );
    let next_unavailable = unavailable(
        state.navigation_unavailable_reason.as_deref(),
        "next_unavailable",
        "imager-connected-error-next-unavailable",
    );
    let mut root = Resource::new(
        CONNECTION_RESOURCE_ID,
        "imager.connections",
        strings::t("imager-connected-resource-root"),
    )
    .with_presentation(ResourcePresentation {
        preview: None,
        detail: Some(strings::t_args(
            "imager-connected-detail-root",
            &[
                ("phase", strings::t(state.phase.detail_key()).into()),
                ("connections", state.rows.len().into()),
            ],
        )),
        badge: Some(strings::t("imager-connected-badge-root")),
    })
    .with_action(action(
        "back",
        "imager-connected-action-back",
        ConnectionOperation::BackToImports { generation },
        true,
        "stale_route",
        "imager-connected-error-stale-route",
    ))
    .with_action(action(
        "observe",
        "imager-connected-action-observe",
        ConnectionOperation::Observe { generation },
        true,
        "stale_route",
        "imager-connected-error-stale-route",
    ))
    .with_action(action(
        "reload-connections",
        "imager-connected-action-reload",
        ConnectionOperation::ReloadConnections { generation },
        state.reload_available,
        reload_unavailable.0,
        reload_unavailable.1,
    ))
    .with_action(action(
        "first-connections",
        "imager-connected-action-first",
        ConnectionOperation::FirstConnections { generation },
        state.first_available,
        first_unavailable.0,
        first_unavailable.1,
    ))
    .with_action(action(
        "next-connections",
        "imager-connected-action-next",
        ConnectionOperation::NextConnections { generation },
        state.next_available,
        next_unavailable.0,
        next_unavailable.1,
    ));
    root = root.with_children(state.rows.iter().map(|row| row_resource(state, row)));
    root
}

#[cfg(test)]
fn projection_text(resource: &Resource) -> String {
    fn collect(resource: &Resource, text: &mut String) {
        use std::fmt::Write as _;
        let _ = write!(text, "{} {} ", resource.label, resource.kind);
        if let Some(presentation) = &resource.presentation {
            let _ = write!(
                text,
                "{} {} ",
                presentation.detail.as_deref().unwrap_or(""),
                presentation.badge.as_deref().unwrap_or("")
            );
        }
        for action in &resource.actions {
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            let _ = write!(
                text,
                "{} {} {} ",
                action.label,
                encoded.type_id,
                String::from_utf8_lossy(&encoded.payload)
            );
        }
        for child in &resource.children {
            collect(child, text);
        }
    }
    let mut text = String::new();
    collect(resource, &mut text);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_controls_are_effects_but_closed_while_navigation_stays_view() {
        let root = resource(&ManagementProjection {
            generation: 7,
            phase: ManagementPhase::Catalog,
            status_code: "catalog".into(),
            status_is_error: false,
            rows: vec![ConnectionProjection {
                alias: 11,
                number: 1,
                revision: 3,
                lifecycle: "active".into(),
                has_baseline: true,
                selected: true,
                open_settings_available: true,
                open_settings_unavailable_reason: None,
            }],
            selected: Some(SelectedProjection {
                alias: 11,
                phase: "paused".into(),
                verified_steps: 4,
                pending_operation: false,
                endpoint_issue: None,
                awaiting_entries: None,
            }),
            reload_available: true,
            next_available: false,
            first_available: true,
            navigation_unavailable_reason: None,
        });

        let roots = [root];
        let actions = tilo_actions::flatten(&roots);
        for id in [
            "sync-now",
            "pause",
            "disconnect",
            "accept-initial",
            "restore",
            "replace",
        ] {
            let action = actions.iter().find(|entry| entry.action.id == id).unwrap();
            assert_eq!(action.action.kind, tilo_actions::OperationKind::Effect);
            assert!(matches!(
                action.action.contract.availability,
                tilo_actions::Availability::Unavailable { .. }
            ));
        }
        for id in ["back", "observe", "first-connections", "open-settings"] {
            let action = actions.iter().find(|entry| entry.action.id == id).unwrap();
            assert_eq!(action.action.kind, tilo_actions::OperationKind::View);
        }
        assert!(actions.iter().all(|entry| entry.action.id != "create"));
        let exposed = projection_text(&roots[0]);
        assert!(!exposed.contains("baseline true"));
        assert!(!exposed.contains("baseline false"));
        assert!(exposed.contains("prior comparison recorded"));
    }

    #[test]
    fn decoder_rejects_extra_fields_wrong_kind_and_oversized_string_aliases() {
        let extra = br#"{"operation":"observe","extra":true}"#;
        assert!(decode(CONNECTION_OPERATION_TYPE, extra).is_err());

        let string_alias = format!(
            "{{\"operation\":\"open-settings\",\"generation\":7,\"alias\":\"{}\"}}",
            "x".repeat(4096)
        );
        assert!(decode(CONNECTION_OPERATION_TYPE, string_alias.as_bytes()).is_err());

        let encoded = encode(&ConnectionOperation::SyncNow {
            generation: 7,
            alias: 11,
        });
        let operation = decode(&encoded.type_id, &encoded.payload).unwrap().unwrap();
        assert_eq!(
            expected_kind(&operation),
            tilo_actions::OperationKind::Effect
        );
        assert!(validate_kind(&operation, tilo_actions::OperationKind::View).is_err());
    }

    #[test]
    fn exported_projection_never_contains_local_paths_or_native_ids() {
        let root = resource(&ManagementProjection {
            generation: 9,
            phase: ManagementPhase::Settings,
            status_code: "/private/source/SENTINEL".into(),
            status_is_error: false,
            rows: Vec::new(),
            selected: None,
            reload_available: true,
            next_available: false,
            first_available: true,
            navigation_unavailable_reason: None,
        });
        let exposed = projection_text(&root);
        assert!(!exposed.contains("/private/source/SENTINEL"));
        assert!(!exposed.contains("sha256"));
        assert!(!exposed.contains("review"));
    }
}
