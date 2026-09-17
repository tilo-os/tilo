use serde::{Deserialize, Serialize};
use ui::tilo_actions::{ActionSpec, EncodedAction, ExecutionLocus, OperationKind, Resource};

use accounts::SecretItem;

pub const OPERATION_TYPE: &str = "tilo.passwords.operation";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PasswordOperation {
    Add,
    SaveDraft,
    CancelDraft,
    GeneratePassword,
    Edit { id: String },
    Copy { id: String },
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

pub fn vault_resource(form_open: bool) -> Resource {
    let mut resource = Resource::new("passwords:vault", "passwords.vault", "Password vault")
        .with_action(operation(
            "add",
            "New item",
            OperationKind::View,
            &PasswordOperation::Add,
        ))
        .with_action(operation(
            "import",
            "Import passwords",
            OperationKind::Effect,
            &PasswordOperation::ImportCsv,
        ))
        .with_action(operation(
            "export",
            "Export CSV…",
            OperationKind::Effect,
            &PasswordOperation::ExportCsv,
        ));
    if form_open {
        resource = resource
            .with_action(operation(
                "save",
                "Save",
                OperationKind::Effect,
                &PasswordOperation::SaveDraft,
            ))
            .with_action(operation(
                "cancel",
                "Cancel editing",
                OperationKind::View,
                &PasswordOperation::CancelDraft,
            ))
            .with_action(operation(
                "generate",
                "Generate strong password",
                OperationKind::Effect,
                &PasswordOperation::GeneratePassword,
            ));
    }
    resource
}

pub fn item_resource(item: &SecretItem) -> Resource {
    let id = item.id.0.clone();
    let (resource_type, copy_label) = match item.kind {
        accounts::SecretItemKind::Login => ("passwords.login", "Copy password"),
        accounts::SecretItemKind::ApiKey => ("passwords.api-key", "Copy API key"),
        accounts::SecretItemKind::SecureNote => ("passwords.note", "Copy note"),
        accounts::SecretItemKind::Wifi => ("passwords.wifi", "Copy Wi-Fi password"),
        accounts::SecretItemKind::Other => ("passwords.secret", "Copy secret"),
    };
    Resource::new(
        format!("passwords:item:{id}"),
        resource_type,
        item.title.clone(),
    )
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
        "delete",
        "Delete…",
        OperationKind::Effect,
        &PasswordOperation::RequestDelete { id },
    ))
}
