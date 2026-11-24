//! What a workspace's vault publishes at `workspace/vault`: the person's items
//! and the entries of their Passwords, as actions of the workspace's own tree,
//! reached by a path as every other action is. Each is decided by the
//! workspace's policy on its vault as any call of it is; what an agent asks by
//! reference is offered elsewhere ([`person_vault::use_rows`]).

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_VAULT, person_vault, workspace_governance_child,
    workspace_governance_resource,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::Verb;
use crate::workspaces::workspace_stores::Store;

/// The `tilo.vault` actions a person asks of the workspace's vault, in
/// catalogue order.
const VERBS: [Verb; 8] = [
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
        id: person_vault::ENTRIES,
        label: "List Passwords entries",
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: person_vault::KEEP_ENTRY,
        label: "Keep a Passwords entry",
        check: "",
        kind: ActionKind::Undoable,
        params: keep_entry_params,
    },
    Verb {
        id: person_vault::FORGET_ENTRY,
        label: "Forget a Passwords entry",
        check: "",
        kind: ActionKind::Undoable,
        params: forget_entry_params,
    },
];

/// Each verb and the `tilo.vault` action it is.
const VAULT_VERBS: [(&Verb, &str); 8] = [
    (&VERBS[0], person_vault::LIST),
    (&VERBS[1], person_vault::ADD),
    (&VERBS[2], person_vault::REMOVE),
    (&VERBS[3], person_vault::USES),
    (&VERBS[4], person_vault::TAKE_BACK),
    (&VERBS[5], person_vault::ENTRIES),
    (&VERBS[6], person_vault::KEEP_ENTRY),
    (&VERBS[7], person_vault::FORGET_ENTRY),
];

fn add_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "kind",
            ParamType::Text,
            "What it is: `login`, `key`, `code-generator`, `ssh-key` or `value`.",
        ),
        ActionParam::required(
            "belongsTo",
            ParamType::Array,
            "Where it belongs, each a web address such as https://example.com or a server such \
             as imap://mail.example.com:993.",
        ),
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
            "tags",
            ParamType::Array,
            "What the item is tagged with. A secret an app was moved to is tagged with the app \
             and purpose it was kept for, and the app finds it by those.",
        ),
        ActionParam::required(
            "secret",
            ParamType::Secret,
            "The secret: sealed in the workspace's vault, never answered back.",
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

fn keep_entry_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "entry",
        ParamType::Object,
        "The entry of Passwords: its id, title, name, password, notes, fields, tags and \
         origins, as Passwords reads and writes it.",
    )]
}

fn forget_entry_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "id",
        ParamType::Text,
        "The id of the Passwords entry to forget.",
    )]
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
