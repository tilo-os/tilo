//! Focused-field metadata. References never grant permission or execution authority.
//! The ordinary resource/action owner remains the only action implementation.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionAddress, OperationKind};

pub const MAX_ACTIONS: usize = 6;
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024;
pub const MAX_LABEL_BYTES: usize = 96;

/// Application-side inert reference to a fully bound canonical invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundAction {
    pub address: ActionAddress,
    /// Optional bound-choice caption; the owner's action contract remains canonical.
    #[serde(default)]
    pub label: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// One private field snapshot. `revision` fences canonical owner admission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSnapshot {
    pub field: String,
    pub app: String,
    pub room: String,
    pub record: String,
    pub revision: String,
    pub actions: Vec<BoundAction>,
}

impl FieldSnapshot {
    /// Decode bounded metadata; malformed declarations cannot become a context.
    ///
    /// # Errors
    /// Refuses malformed, oversized, ambiguous or duplicate action references.
    pub fn decode(json: &str) -> Result<Self, String> {
        if json.len() > MAX_SNAPSHOT_BYTES {
            return Err("focused input context is too large".into());
        }
        let snapshot: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        let strings = [
            &snapshot.field,
            &snapshot.app,
            &snapshot.room,
            &snapshot.record,
            &snapshot.revision,
        ];
        if strings
            .iter()
            .any(|value| value.len() > 512 || value.contains(['\0', '\n', '\r']))
            || snapshot.field.is_empty()
            || snapshot.revision.is_empty()
            || snapshot.actions.len() > MAX_ACTIONS
        {
            return Err("focused input context is invalid".into());
        }
        for (index, action) in snapshot.actions.iter().enumerate() {
            if action.label.len() > MAX_LABEL_BYTES || action.label.chars().any(char::is_control) {
                return Err("focused input action label is invalid".into());
            }
            if snapshot
                .actions
                .iter()
                .take(index)
                .any(|previous| previous == action)
            {
                return Err("focused input context repeats an action".into());
            }
        }
        Ok(snapshot)
    }
}

/// Keyboard-side presentation. App addresses, arguments and values stay private.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSlot {
    pub slot: u32,
    pub label: String,
    pub kind: OperationKind,
}

#[cfg(feature = "client")]
pub use wayland_client;

#[cfg(unix)]
pub mod wait;

#[cfg(feature = "client")]
pub mod client {
    use wayland_client;
    use wayland_client::protocol::wl_surface;
    pub mod __interfaces {
        use wayland_backend;
        use wayland_client::protocol::__interfaces::{WL_SURFACE_INTERFACE, wl_surface_interface};
        wayland_scanner::generate_interfaces!("protocol/tilo-input-context-v1.xml");
    }
    use self::__interfaces::{
        TILO_INPUT_CONTEXT_FIELD_V1_INTERFACE, TILO_INPUT_CONTEXT_KEYBOARD_V1_INTERFACE,
        TILO_INPUT_CONTEXT_MANAGER_V1_INTERFACE,
    };
    wayland_scanner::generate_client_code!("protocol/tilo-input-context-v1.xml");
}

#[cfg(feature = "server")]
pub mod server {
    use wayland_server;
    use wayland_server::protocol::wl_surface;
    pub mod __interfaces {
        use wayland_backend;
        use wayland_server::protocol::__interfaces::{WL_SURFACE_INTERFACE, wl_surface_interface};
        wayland_scanner::generate_interfaces!("protocol/tilo-input-context-v1.xml");
    }
    use self::__interfaces::{
        TILO_INPUT_CONTEXT_FIELD_V1_INTERFACE, TILO_INPUT_CONTEXT_KEYBOARD_V1_INTERFACE,
        TILO_INPUT_CONTEXT_MANAGER_V1_INTERFACE,
    };
    wayland_scanner::generate_server_code!("protocol/tilo-input-context-v1.xml");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_bound_edits_share_the_original_canonical_action() {
        let json = r#"{"field":"body","app":"notes","room":"room","record":"notebook","revision":"v5:current","actions":[{"address":{"resource":"page/body","action":"apply"},"arguments":{"mark":"bold"}},{"address":{"resource":"page/body","action":"apply"},"arguments":{"mark":"italic"}}]}"#;
        let snapshot = FieldSnapshot::decode(json).unwrap();
        assert_eq!(snapshot.actions.len(), 2);
        assert_eq!(snapshot.actions[0].address, snapshot.actions[1].address);
        let mut duplicate = snapshot;
        duplicate.actions[1] = duplicate.actions[0].clone();
        assert!(FieldSnapshot::decode(&serde_json::to_string(&duplicate).unwrap()).is_err());
    }

    #[test]
    fn malformed_and_oversized_app_metadata_is_refused() {
        assert!(FieldSnapshot::decode(&"x".repeat(MAX_SNAPSHOT_BYTES + 1)).is_err());
        let json = r#"{"field":"body\nsecret","app":"notes","room":"room","record":"notebook","revision":"v5:current","actions":[]}"#;
        assert!(FieldSnapshot::decode(json).is_err());
        let json = r#"{"field":"body","app":"notes","room":"room","record":"notebook","revision":"v5:current","actions":[],"authority":"write"}"#;
        assert!(FieldSnapshot::decode(json).is_err());
    }

    #[test]
    fn keyboard_projection_carries_no_resource_or_document_argument() {
        let slot = ContextSlot {
            slot: 0,
            label: "Bold".into(),
            kind: OperationKind::Document,
        };
        let wire = serde_json::to_value(slot).unwrap();
        let fields = wire.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields.contains_key("slot"));
        assert!(fields.contains_key("label"));
        assert!(fields.contains_key("kind"));
        assert!(!fields.contains_key("address"));
        assert!(!fields.contains_key("arguments"));
    }
}
