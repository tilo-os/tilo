//! What a workspace's connected accounts publish to programs: their reading (`accounts.accounts`
//! of `tilo.accounts`), each account's kind, address, provider and how its last sync went, and
//! the verbs that connect one, sign one in again and remove it. The file beside this crate
//! ([`accounts-surface.json`]) is what `tilolang check` compiles a view against and a host loads
//! the package from; the test here writes it, so the two cannot drift.

use tilo_actions::remote::person_accounts;
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::Verb;
use super::mail;

/// The `tilo.accounts` actions a person asks of the workspace's accounts, in catalogue order.
const VERBS: [Verb; 5] = [
    Verb {
        id: person_accounts::ADD_MAIL,
        label: "Add a mail account",
        check: "",
        kind: ActionKind::Undoable,
        params: mail::add_account_params,
    },
    Verb {
        id: person_accounts::RECONNECT,
        label: "Sign an account in again",
        check: "",
        kind: ActionKind::Undoable,
        params: account_params,
    },
    Verb {
        id: person_accounts::CHECK,
        label: "Check an account's server",
        check: "",
        kind: ActionKind::Read,
        params: account_params,
    },
    Verb {
        id: person_accounts::REPLACE_PASSWORD,
        label: "Replace an account's sign-in",
        check: "",
        kind: ActionKind::Undoable,
        params: password_params,
    },
    Verb {
        id: person_accounts::REMOVE,
        label: "Remove an account",
        check: "",
        kind: ActionKind::Undoable,
        params: account_params,
    },
];

fn password_params() -> Vec<ActionParam> {
    let mut params = account_params();
    params.push(ActionParam::required(
        "password",
        ParamType::Secret,
        "The password or app password the server took: sealed in the workspace's vault, \
         never answered back.",
    ));
    params
}

fn account_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "account",
        ParamType::Text,
        "The account's id, as the accounts list names it.",
    )]
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    /// A workspace's accounts as a view of another language reads and changes them
    /// (`accounts.accounts`): the reading's fields, the records it holds and the verbs a view
    /// calls. It is the file beside this crate that `tilolang check` compiles a view against and
    /// a host loads the package from; `UPDATE_SNAPSHOTS=1` writes it from this one source.
    #[must_use]
    pub fn surface() -> tilo_actions::surface::Surface {
        use tilo_actions::field_type::{FieldType, NamedType, TypeCase, TypeField};
        use tilo_actions::surface::{FieldSurface, Surface};

        let reading = |path: &str, ty: FieldType, title: &str| FieldSurface {
            path: path.to_owned(),
            owner: String::new(),
            title: title.to_owned(),
            stored: "value".to_owned(),
            ty,
            readonly: true,
            unit: false,
            label: false,
            whole: false,
            ops: None,
        };
        let named = FieldType::Named;
        let list = |ty: FieldType| FieldType::List(Box::new(ty));
        let record = |name: &str, doc: &str, fields: &[(&str, FieldType, &str)]| NamedType {
            name: name.to_owned(),
            doc: doc.to_owned(),
            fields: fields
                .iter()
                .map(|(name, ty, title)| TypeField {
                    name: (*name).to_owned(),
                    ty: ty.clone(),
                    title: (*title).to_owned(),
                })
                .collect(),
            cases: Vec::new(),
        };
        // One case per value the reading serialises the kind as.
        let kind = NamedType {
            name: "AccountKind".to_owned(),
            doc: "The kind of service an account is.".to_owned(),
            fields: Vec::new(),
            cases: ["mail", "calendar", "cloud", "other"]
                .into_iter()
                .map(|name| TypeCase {
                    name: name.to_owned(),
                    label: match name {
                        "mail" => "Mail".to_owned(),
                        "calendar" => "Calendar".to_owned(),
                        "cloud" => "Cloud storage".to_owned(),
                        _ => "Other".to_owned(),
                    },
                    fields: Vec::new(),
                })
                .collect(),
        };
        // One case per value the reading serialises the sync status as.
        let status = NamedType {
            name: "AccountStatus".to_owned(),
            doc: "How an account's last sync went, as its row says.".to_owned(),
            fields: Vec::new(),
            cases: [
                ("connected", "Up to date"),
                ("needs-reconnect", "Sign-in required"),
                ("error", "Sync failed"),
                ("waiting", "Waiting for a connection"),
                ("syncing", "Syncing"),
                ("never", "Not synced yet"),
            ]
            .into_iter()
            .map(|(name, label)| TypeCase {
                name: name.to_owned(),
                label: label.to_owned(),
                fields: Vec::new(),
            })
            .collect(),
        };
        Surface {
            kind: "accounts".to_owned(),
            label: "Connected accounts".to_owned(),
            version: 1,
            person: false,
            readers: None,
            verbs: VERBS.iter().map(verb_surface).collect(),
            fields: vec![reading(
                "accounts",
                list(named("AccountView".to_owned())),
                "The accounts the workspace connected, in the record's order.",
            )],
            types: vec![
                record(
                    "AccountView",
                    "One account the workspace connected, without its secret.",
                    &[
                        (
                            "id",
                            FieldType::Text,
                            "The account's id, as every action names it.",
                        ),
                        (
                            "kind",
                            named("AccountKind".to_owned()),
                            "The kind of service it is.",
                        ),
                        ("name", FieldType::Text, "What its person calls it."),
                        (
                            "address",
                            FieldType::Text,
                            "The address or name it signs in at.",
                        ),
                        (
                            "username",
                            FieldType::Text,
                            "The name it signs in with.",
                        ),
                        ("provider", FieldType::Text, "The provider it belongs to."),
                        (
                            "status",
                            named("AccountStatus".to_owned()),
                            "How its last sync went.",
                        ),
                        (
                            "words",
                            FieldType::Text,
                            "What a person reads about that, as its device said it.",
                        ),
                        ("enabled", FieldType::Bool, "Whether the account is in use."),
                        (
                            "incoming",
                            FieldType::Text,
                            "A mail account's incoming server as a person writes it.",
                        ),
                        (
                            "outgoing",
                            FieldType::Text,
                            "Its outgoing server as a person writes it.",
                        ),
                        (
                            "identities",
                            list(FieldType::Text),
                            "The other addresses it sends as, each `Name <address>`.",
                        ),
                    ],
                ),
                kind,
                status,
            ],
            shows: Vec::new(),
            live: None,
            setters: false,
        }
    }

    /// One verb as a view of another language calls it: its verb, what it takes and what it is
    /// for.
    fn verb_surface(verb: &Verb) -> tilo_actions::surface::VerbSurface {
        use tilo_actions::field_type::FieldType;
        use tilo_actions::surface::{ParamSurface, VerbSurface};

        let params = (verb.params)()
            .into_iter()
            .map(|param| {
                let ty = match (param.ty, param.name.as_ref()) {
                    (ParamType::Text | ParamType::Secret | ParamType::DateTime, _) => {
                        FieldType::Text
                    }
                    (ParamType::Integer, _) => FieldType::Int,
                    (ParamType::Number, _) => FieldType::Float,
                    (ParamType::Boolean, _) => FieldType::Bool,
                    (ParamType::Array, _) => FieldType::List(Box::new(FieldType::Text)),
                    (ParamType::Object, _) | (ParamType::Resource, _) => FieldType::Any,
                };
                ParamSurface {
                    name: param.name.into_owned(),
                    required: param.required,
                    schema: param.schema.as_deref().cloned().unwrap_or_default(),
                    ty,
                    within: None,
                }
            })
            .collect();
        VerbSurface {
            id: verb.id.to_owned(),
            name: String::new(),
            label: verb.label.to_owned(),
            description: None,
            kind: verb.kind,
            destructive: false,
            incoming_message: false,
            params,
            returns: None,
            writes: vec!["record".to_owned()],
            field: None,
        }
    }

    /// The file a tilolang view compiles a workspace's accounts against is what this crate
    /// serves. `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_accounts_surface_file_is_what_this_crate_publishes() {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": [surface()] }))
            .expect("a surface writes as JSON")
            + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("accounts-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/tilo-workspace-service/accounts-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
