//! What a workspace's vault publishes at `workspace/vault`: the person's items,
//! as actions of the workspace's own tree, reached by a path as every other
//! action is, and by Passwords as its person's window on them. Each is decided by the
//! workspace's policy on its vault as any call of it is; what an agent asks by
//! reference is offered elsewhere ([`person_vault::use_rows`]).

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_VAULT, person_vault, workspace_governance_child,
    workspace_governance_resource,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Publisher, Verb};
use crate::workspaces::workspace_stores::Store;

/// The vault as Passwords reaches it: `tilo.vault`'s own calls, which the app calls by their
/// address (`tilo.vault/vault#list`) as its person, on their device.
pub(super) const VAULT: Publisher = Publisher {
    provider: person_vault::PROVIDER,
    kind: person_vault::RESOURCE,
};

/// The `tilo.vault` actions a person asks of the workspace's vault, in
/// catalogue order.
const VERBS: [Verb; 7] = [
    Verb {
        id: person_vault::LIST,
        label: "List vault items",
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: person_vault::ADD,
        label: "Keep a vault item",
        check: "",
        kind: ActionKind::Undoable,
        params: add_params,
    },
    Verb {
        id: person_vault::REMOVE,
        label: "Forget a vault item",
        check: "",
        kind: ActionKind::Undoable,
        params: item_params,
    },
    Verb {
        id: person_vault::USES,
        label: "Read the uses of a vault item",
        check: "",
        kind: ActionKind::Read,
        params: item_params,
    },
    Verb {
        id: person_vault::TAKE_BACK,
        label: "Take back what may always use a vault item",
        check: "",
        kind: ActionKind::Undoable,
        params: take_back_params,
    },
    Verb {
        id: person_vault::COPY,
        label: "Copy a vault item's secret",
        check: "",
        kind: ActionKind::Read,
        params: show_params,
    },
    Verb {
        id: person_vault::REVEAL,
        label: "Show a vault item's secret",
        check: "",
        kind: ActionKind::Read,
        params: show_params,
    },
];

/// Each verb and the `tilo.vault` action it is.
const VAULT_VERBS: [(&Verb, &str); 7] = [
    (&VERBS[0], person_vault::LIST),
    (&VERBS[1], person_vault::ADD),
    (&VERBS[2], person_vault::REMOVE),
    (&VERBS[3], person_vault::USES),
    (&VERBS[4], person_vault::TAKE_BACK),
    (&VERBS[5], person_vault::COPY),
    (&VERBS[6], person_vault::REVEAL),
];

fn add_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "item",
            ParamType::Resource,
            "The item this changes, by its reference (`vault.item:…`): it keeps its reference and \
             its uses, and each of its secrets, its username, title, note and field values left \
             out stays as it was. Left out for a new item.",
        )
        .of_resource_kind(person_vault::ITEM_KIND),
        ActionParam::required(
            "kind",
            ParamType::Text,
            "What it is: `login`, `key`, `code-generator`, `ssh-key`, `note`, `wifi` or `value`.",
        ),
        ActionParam::optional(
            "belongsTo",
            ParamType::Array,
            "Where it belongs, each a web address such as https://example.com or a server such \
             as imap://mail.example.com:993. A note, a Wi-Fi password or a licence code may \
             belong nowhere.",
        ),
        ActionParam::optional("title", ParamType::Text, "What it is called."),
        ActionParam::optional("username", ParamType::Text, "The name it signs in with."),
        ActionParam::optional(
            "title",
            ParamType::Text,
            "What it is called: what a person picks it by.",
        ),
        ActionParam::optional(
            "sentIn",
            ParamType::Text,
            "For a key, how its service takes it when not as a bearer token: `basic`, \
             `header:<name>` or `query:<name>`.",
        ),
        ActionParam::optional(
            "secret",
            ParamType::Secret,
            "The secret: sealed in the workspace's vault, never answered back. Left out where \
             `generate` draws a login's password.",
        ),
        ActionParam::optional(
            "notes",
            ParamType::Secret,
            "A note kept with it, sealed as the secret is.",
        ),
        ActionParam::optional(
            "fields",
            ParamType::Secret,
            "The fields kept with it, such as a PIN, each `{label, value}`: each value sealed as \
             its secret is, only the labels ever answered back.",
        )
        .with_schema(serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {"label": {"type": "string"}, "value": {"type": "string"}},
                "required": ["label", "value"],
            },
        })),
        ActionParam::optional(
            "tags",
            ParamType::Array,
            "What the item is tagged with. A secret an app was moved to is tagged with the app \
             and purpose it was kept for, and the app finds it by those.",
        ),
        ActionParam::optional(
            "generate",
            ParamType::Object,
            "For a login without a secret: draw a strong password where the vault is, `length` \
             (12 to 128, 20 when not said) characters, with `capitals`, `digits` and `symbols` \
             unless one is false.",
        ),
    ]
}

fn show_params() -> Vec<ActionParam> {
    vec![
        person_vault::item_param("item", "The item, by its reference (`vault.item:…`)."),
        ActionParam::optional(
            "part",
            ParamType::Text,
            "Which part: `secret` (the password, key or value; a note's text) when not said, \
             `notes`, `username` or `field:<label>`.",
        ),
        ActionParam::optional(
            "notes",
            ParamType::Secret,
            "A note kept with it: sealed as its secret is, never answered back.",
        ),
        ActionParam::optional(
            "fields",
            ParamType::Secret,
            "The fields kept with it, such as a PIN, each `{label, value}`: each value sealed as \
             its secret is, only the labels ever answered back.",
        )
        .with_schema(serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {"label": {"type": "string"}, "value": {"type": "string"}},
                "required": ["label", "value"],
            },
        })),
    ]
}

/// The parameters the vault's action `action` (a `tilo.vault` action, or a verb of the vault in
/// a workspace's governance tree) is published with.
pub(super) fn params_of(action: &str) -> Option<Vec<ActionParam>> {
    // An app keeps its own secret on its held connection alone, so no catalogue offers it; it
    // is described all the same, so what keeps a call of it leaves its value out.
    if action == person_vault::KEEP {
        return Some(person_vault::keep_params());
    }
    VAULT_VERBS
        .iter()
        .find(|(verb, named)| verb.id == action || *named == action)
        .map(|(verb, _)| (verb.params)())
}

fn item_params() -> Vec<ActionParam> {
    vec![person_vault::item_param(
        "item",
        "The item, by its reference (`vault.item:…`).",
    )]
}
fn take_back_params() -> Vec<ActionParam> {
    vec![
        person_vault::item_param("item", "The item, by its reference (`vault.item:…`)."),
        ActionParam::optional(
            "agent",
            ParamType::Text,
            "The agent whose leaves to take back.",
        ),
        ActionParam::optional(
            "app",
            ParamType::Text,
            "The app whose leave to read the item to take back. With neither this nor `agent`, \
             every leave of the item goes.",
        ),
    ]
}

/// The `tilo.vault` action `address` asks of a workspace's vault in its
/// governance tree, and the workspace it names.
pub(super) fn vault_action(address: &tilo_actions::ActionAddress) -> Option<(&str, &str)> {
    let workspace = address
        .resource
        .strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?
        .strip_suffix(WORKSPACE_VAULT)?
        .strip_suffix('/')?;
    let action = VAULT_VERBS
        .iter()
        .find(|(verb, _)| verb.id == address.action)
        .map(|(_, action)| *action)?;
    Some((workspace, action))
}

impl crate::workspaces::Workspaces {
    /// The rows of `workspace`'s vault (its UUID) `asked` is offered: those the
    /// workspace's policy on its vault lets it ask.
    pub(super) async fn vault_rows(
        &self,
        asked: super::Asked<'_>,
        workspace: &str,
    ) -> Vec<CatalogueRow> {
        let root = workspace_governance_resource(workspace);
        let resource = workspace_governance_child(workspace, Some(WORKSPACE_VAULT));
        let mut rows = Vec::new();
        for (verb, action) in VAULT_VERBS {
            if self
                .reaching(asked, Store::VAULT, action, verb.kind)
                .await
                .is_ok()
            {
                let mut row = verb.row(super::GOVERNANCE, &resource, "Vault");
                row.parent_resource.clone_from(&root);
                if let Some(descriptor) = row.action.as_mut() {
                    descriptor.effects = person_vault::effects(action);
                }
                rows.push(row);
            }
        }
        rows
    }

    /// The rows of the vault Passwords is offered as `asked`, by its own provider: each call of
    /// it the workspace's policy lets its person ask. No other caller is offered them here.
    pub(super) async fn passwords_rows(&self, asked: super::Asked<'_>) -> Vec<CatalogueRow> {
        let (_, caller, _) = asked;
        if caller
            .acting()
            .app()
            .is_none_or(|app| app.as_str() != tilo_app_id::apps::PASSWORDS)
        {
            return Vec::new();
        }
        let mut rows = Vec::new();
        for (verb, action) in VAULT_VERBS {
            if self
                .reaching(asked, Store::VAULT, action, verb.kind)
                .await
                .is_ok()
            {
                rows.push(verb.row(VAULT, person_vault::RESOURCE, "Vault"));
            }
        }
        rows
    }

    /// Run `request`, one of the workspace's vault actions in its governance
    /// tree, as the `tilo.vault` action `action` it is, in the workspace
    /// `workspace` (its UUID) the request came through.
    pub(super) async fn vault_invoke(
        &self,
        asked: super::Asked<'_>,
        (workspace, action): (&str, &str),
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        let (workspace_id, caller, certificate) = asked;
        let here = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => None,
        };
        if here.as_deref() != Some(workspace) {
            return Err(tilo_actions::remote::ActionCallError::UnknownAction);
        }
        let mut asking = request.clone();
        asking.target = person_vault::target(action);
        self.person_vault_invoke(workspace_id, caller, certificate, &asking)
            .await
    }
}
