//! Local vault Effects and the live item catalogue.
//!
//! Shared filter/search/selection preferences are document actions on
//! [`crate::resource::Passwords`]. Add / import / export are Effects with no
//! document delta. Item rows expose edit / copy / reveal / delete for agents;
//! the widget also commits selection into the shared document when focusing.

use serde::{Deserialize, Serialize};
use ui::tilo_actions::{ActionSpec, EncodedAction, ExecutionLocus, OperationKind, Resource};

use accounts::SecretItem;

use crate::resource::{Passwords, VaultState};

pub const OPERATION_TYPE: &str = "tilo.passwords.operation";
pub const VAULT_RESOURCE_ID: &str = "passwords:vault";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PasswordOperation {
    Add,
    SaveDraft,
    CancelDraft,
    GeneratePassword,
    Open { id: String },
    Edit { id: String },
    Copy { id: String },
    Reveal { id: String },
    RequestDelete { id: String },
    ImportCsv,
    ExportCsv,
}

fn operation(id: &str, label: &str, kind: OperationKind, value: &PasswordOperation) -> ActionSpec {
    ActionSpec::operation(id, label, kind, OPERATION_TYPE, value).unwrap_or_else(|_| {
        ActionSpec::immediate_encoded(
            id,
            label,
            ExecutionLocus::ClientLocal,
            EncodedAction {
                type_id: "tilo.invalid-arguments".into(),
                payload: Vec::new(),
            },
        )
    })
}

/// Vault-level Effects / Views with no document delta (always published).
pub fn vault_effect_specs() -> Vec<ActionSpec> {
    vec![
        operation(
            "add",
            "New item",
            OperationKind::View,
            &PasswordOperation::Add,
        ),
        operation(
            "import",
            "Import passwords",
            OperationKind::Effect,
            &PasswordOperation::ImportCsv,
        ),
        operation(
            "export",
            "Export CSV…",
            OperationKind::Effect,
            &PasswordOperation::ExportCsv,
        ),
    ]
}

/// Editor-session Effects / Views overlaid when a draft is open.
pub fn editor_session_specs() -> Vec<ActionSpec> {
    vec![
        operation(
            "save",
            "Save",
            OperationKind::Effect,
            &PasswordOperation::SaveDraft,
        ),
        operation(
            "cancel",
            "Cancel editing",
            OperationKind::View,
            &PasswordOperation::CancelDraft,
        ),
        operation(
            "generate",
            "Generate strong password",
            OperationKind::Effect,
            &PasswordOperation::GeneratePassword,
        ),
    ]
}

pub fn item_resource(item: &SecretItem) -> Resource {
    let id = item.id.0.clone();
    let (resource_type, resource_name, copy_label, reveal_label) = match item.kind {
        accounts::SecretItemKind::Login => (
            "passwords.login",
            "Saved login",
            "Copy password",
            "Reveal password for 10 seconds",
        ),
        accounts::SecretItemKind::ApiKey => (
            "passwords.api-key",
            "Saved API key",
            "Copy API key",
            "Reveal API key for 10 seconds",
        ),
        accounts::SecretItemKind::SecureNote => (
            "passwords.note",
            "Saved secure note",
            "Copy note",
            "Reveal note for 10 seconds",
        ),
        accounts::SecretItemKind::Wifi => (
            "passwords.wifi",
            "Saved Wi-Fi password",
            "Copy Wi-Fi password",
            "Reveal password for 10 seconds",
        ),
        accounts::SecretItemKind::Other => (
            "passwords.secret",
            "Saved secret",
            "Copy secret",
            "Reveal secret for 10 seconds",
        ),
    };
    // Resources feed system-wide search and collaboration. The encrypted
    // vault metadata (including a title, account and origin) is intentionally
    // not copied into this public descriptor; only an opaque item id is used
    // in the client-local action payload, and no action carries a secret.
    Resource::new(item_resource_id(&item.id), resource_type, resource_name)
        .with_action(operation(
            "open",
            "Open",
            OperationKind::View,
            &PasswordOperation::Open { id: id.clone() },
        ))
        .with_action(operation(
            "edit",
            "Edit",
            OperationKind::View,
            &PasswordOperation::Edit { id: id.clone() },
        ))
        .with_action(operation(
            "copy",
            copy_label,
            OperationKind::Effect,
            &PasswordOperation::Copy { id: id.clone() },
        ))
        .with_action(operation(
            "reveal",
            reveal_label,
            OperationKind::View,
            &PasswordOperation::Reveal { id: id.clone() },
        ))
        .with_action(operation(
            "delete",
            "Delete…",
            OperationKind::Effect,
            &PasswordOperation::RequestDelete { id },
        ))
}

/// The stable resource id of a vault item.
pub fn item_resource_id(id: &accounts::SecretId) -> String {
    format!("passwords:item:{}", id.0)
}

/// The action a search hit for `id` runs: show the item.
pub fn open_item_address(id: &accounts::SecretId) -> ui::tilo_actions::ActionAddress {
    ui::tilo_actions::ActionAddress::new(item_resource_id(id), "open")
}

/// Live vault item rows (no `passwords.vault` root).
pub fn live_item_resources(items: &[SecretItem]) -> Vec<Resource> {
    items.iter().map(item_resource).collect()
}

/// Standalone / `--shots` overlay: [`tilo_resource::publish`] root plus live
/// item children, with editor-session ops when drafting.
pub fn standalone_resources(
    items: &[SecretItem],
    editing: bool,
    state: &VaultState,
) -> Vec<Resource> {
    let mut root =
        ui::tilo_resource::publish::<Passwords>(VAULT_RESOURCE_ID, "Password vault", state);
    if editing {
        for spec in editor_session_specs() {
            root = root.with_action(spec);
        }
    }
    root.children = live_item_resources(items);
    vec![root]
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::SecretItem;
    use ui::tilo_actions::ActionRegistry;

    #[test]
    fn item_resources_keep_encrypted_metadata_out_of_public_descriptors() {
        let mut item = SecretItem::login("Private bank", "ada@private.example", "not-public");
        item.add_origin("https://private.example").unwrap();
        let resource = item_resource(&item);
        let mut public = format!("{} {} {}", resource.id.0, resource.kind, resource.label);
        for action in &resource.actions {
            public.push_str(&action.label);
            let encoded = action
                .build_args(Vec::<(String, ui::tilo_actions::ArgValue)>::new())
                .unwrap();
            public.push_str(&String::from_utf8_lossy(&encoded.payload));
        }

        assert_eq!(resource.label, "Saved login");
        for private in [
            "Private bank",
            "ada@private.example",
            "private.example",
            "not-public",
        ] {
            assert!(!public.contains(private), "resource leaked {private:?}");
        }
        assert!(public.contains("Reveal"));
    }

    // Shared filter declarations plus Effects form the palette door; item
    // ops ride the standalone overlay's live children.
    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: {
            let mut registry = ActionRegistry::new();
            <Passwords as ui::tilo_resource::Resource>::register(&mut registry);
            registry
        },
        resources: |doc| {
            let mut core = crate::vault::Core::demo();
            core.begin_edit();
            core.publish_resources(doc)
        },
        palette: ["New item", "Import passwords", "Generate strong password", "Copy password", "Filter vault"],
    }
}
