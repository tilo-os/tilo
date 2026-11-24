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
const VERBS: [Verb; 12] = [
    Verb {
        id: person_accounts::ADD_MAIL,
        label: "Add a mail account",
        check: "",
        kind: ActionKind::Undoable,
        params: mail::add_account_params,
    },
    Verb {
        id: person_accounts::BEGIN_SIGN_IN,
        label: "Sign in to an account on its provider's page",
        check: "",
        kind: ActionKind::Undoable,
        params: begin_sign_in_params,
    },
    Verb {
        id: person_accounts::BEGIN_CLOUD_SIGN_IN,
        label: "Connect cloud storage",
        check: "",
        kind: ActionKind::Undoable,
        params: begin_cloud_sign_in_params,
    },
    Verb {
        id: person_accounts::SUBMIT_SIGN_IN_CODE,
        label: "Finish signing in with a code",
        check: "",
        kind: ActionKind::Undoable,
        params: code_params,
    },
    Verb {
        id: person_accounts::CANCEL_SIGN_IN,
        label: "Stop waiting for a sign-in",
        check: "",
        kind: ActionKind::Undoable,
        params: sign_in_params,
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
        id: person_accounts::DISCOVER_CALENDARS,
        label: "Find an account's calendars",
        check: "",
        kind: ActionKind::Undoable,
        params: account_params,
    },
    Verb {
        id: person_accounts::SET_CALENDARS,
        label: "Choose an account's calendars",
        check: "",
        kind: ActionKind::Undoable,
        params: calendar_params,
    },
    Verb {
        id: person_accounts::SET_SERVICES,
        label: "Choose an account's services",
        check: "",
        kind: ActionKind::Undoable,
        params: services_params,
    },
    Verb {
        id: person_accounts::REMOVE,
        label: "Remove an account",
        check: "",
        kind: ActionKind::Undoable,
        params: account_params,
    },
];

fn begin_sign_in_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "provider",
            ParamType::Text,
            "The provider: `nextcloud`, `google`, `microsoft`, `icloud` or `other`.",
        ),
        ActionParam::optional(
            "server",
            ParamType::Text,
            "The Nextcloud server's address; left out for the others.",
        ),
        ActionParam::optional(
            "name",
            ParamType::Text,
            "The name the account is shown under.",
        ),
        ActionParam::optional(
            "mail",
            ParamType::Boolean,
            "Whether its mail shows in Mail.",
        ),
        ActionParam::optional(
            "calendar",
            ParamType::Boolean,
            "Whether its calendars show in Calendar.",
        ),
        ActionParam::optional(
            "files",
            ParamType::Boolean,
            "Whether its files mount in Files.",
        ),
        ActionParam::optional(
            "account",
            ParamType::Text,
            "The account this signs in again, when its sign-in stopped working.",
        ),
    ]
}

fn begin_cloud_sign_in_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "kind",
            ParamType::Text,
            "The storage: `google-drive`, `onedrive`, `dropbox`, `icloud-drive` or `webdav`.",
        ),
        ActionParam::optional(
            "name",
            ParamType::Text,
            "The name the account is shown under.",
        ),
        ActionParam::optional(
            "server",
            ParamType::Text,
            "A `WebDAV` server's address.",
        ),
        ActionParam::optional(
            "username",
            ParamType::Text,
            "The sign-in name: a `WebDAV` server's, or the Apple ID.",
        ),
        ActionParam::optional(
            "password",
            ParamType::Secret,
            "Its password or app password, used once and never answered back.",
        ),
    ]
}

fn sign_in_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "signIn",
        ParamType::Text,
        "The sign-in's id, as beginning it answered.",
    )]
}

fn code_params() -> Vec<ActionParam> {
    let mut params = sign_in_params();
    params.push(ActionParam::required(
        "code",
        ParamType::Text,
        "The code the provider sent or showed.",
    ));
    params
}

fn services_params() -> Vec<ActionParam> {
    let mut params = account_params();
    params.push(ActionParam::optional(
        "mail",
        ParamType::Boolean,
        "Whether its mail shows, where it is a mail account.",
    ));
    params.push(ActionParam::optional(
        "calendar",
        ParamType::Boolean,
        "Whether its calendars show in Calendar.",
    ));
    params.push(ActionParam::optional(
        "files",
        ParamType::Boolean,
        "Whether its files mount in Files.",
    ));
    params
}

fn calendar_params() -> Vec<ActionParam> {
    let mut params = account_params();
    params.push(ActionParam::required(
        "calendars",
        ParamType::Array,
        "The ids of the calendars that show, as the accounts reading names them.",
    ));
    params
}

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
                        (
                            "calendars",
                            list(named("CalendarView".to_owned())),
                            "A calendar account's calendars and which of them show.",
                        ),
                        (
                            "services",
                            named("AccountServices".to_owned()),
                            "Which of its services the account feeds.",
                        ),
                    ],
                ),
                record(
                    "CalendarView",
                    "One of a calendar account's calendars.",
                    &[
                        (
                            "id",
                            FieldType::Text,
                            "Its id, as its server keeps it and the selection names it.",
                        ),
                        ("name", FieldType::Text, "What its server calls it."),
                        (
                            "visible",
                            FieldType::Bool,
                            "Whether its events show in the calendar.",
                        ),
                        (
                            "read_only",
                            FieldType::Bool,
                            "Whether its server lets this workspace change it.",
                        ),
                    ],
                ),
                record(
                    "AccountServices",
                    "Which of one account's services its person uses.",
                    &[
                        (
                            "mail",
                            FieldType::Bool,
                            "Whether its mail shows in Mail.",
                        ),
                        (
                            "calendar",
                            FieldType::Bool,
                            "Whether its calendars show in Calendar.",
                        ),
                        (
                            "files",
                            FieldType::Bool,
                            "Whether its files mount in Files.",
                        ),
                    ],
                ),
                record(
                    "SignInStep",
                    "Where a sign-in goes, and what it waits for.",
                    &[
                        (
                            "id",
                            FieldType::Text,
                            "The sign-in's id, as finishing or stopping it names it.",
                        ),
                        (
                            "open",
                            FieldType::Text,
                            "The provider's page to open in the browser.",
                        ),
                        (
                            "code",
                            FieldType::Text,
                            "What it waits for beyond the browser: a verification code, or the \
                             code a provider showed. Empty when it needs none.",
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
        let returns = matches!(
            verb.id,
            person_accounts::BEGIN_SIGN_IN | person_accounts::BEGIN_CLOUD_SIGN_IN
        )
        .then(|| tilo_actions::surface::ReturnsSurface {
            schema: serde_json::Value::Null,
            ty: FieldType::Named("SignInStep".to_owned()),
        });
        VerbSurface {
            id: verb.id.to_owned(),
            name: String::new(),
            label: verb.label.to_owned(),
            description: None,
            kind: verb.kind,
            destructive: false,
            incoming_message: false,
            params,
            returns,
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
