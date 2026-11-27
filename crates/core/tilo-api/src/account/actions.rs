//! The account's actions, declared once: the person's workspaces, their keys, what the
//! workspaces keep and ran, their own model and the key's webhooks.
//!
//! Each resource's actions are one [`Effect`](tilo_resource::Effect) vocabulary, so the account
//! answers a call it decoded here, and every language reaches it from the surface this module
//! publishes (`account-surface.json`, written by the test below). Nothing a call carries is read
//! twice: [`Asked::decode`] is the one place an address and its arguments become an action.

use serde_json::{Map, Value, json};
use tilo_actions::remote::{ActionCallError, CatalogueResource, CatalogueRow};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ExecutionLocus, InputSpec,
    OperationKind, ResultSpec,
};
use tilo_resource::Effect as _;
use tilo_resource::effect::EffectDecl;

use super::{KEYS, MODELS, PROVIDER, USAGE, WEBHOOKS, WORKSPACES};

/// The person's workspaces: list them, make one, branch one, name one, delete one, and keep one
/// on this device.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.account.workspaces")]
pub enum WorkspaceAction {
    /// List your workspaces
    #[effect(kind = "read")]
    List {
        #[param("Names to text values only the person's keys read: {customer_id: \"cus_8121\"}.")]
        labels: Option<Map<String, Value>>,
    },
    /// Create a workspace
    #[effect(kind = "outside")]
    Create {
        #[param("The workspace's name.")]
        name: String,
        #[param("Names to text values only the person's keys read: {customer_id: \"cus_8121\"}.")]
        labels: Option<Map<String, Value>>,
        #[param("The new workspace's id (a UUID), so a retry asks for the same one.")]
        id: Option<String>,
    },
    /// Branch a workspace of yours
    #[effect(kind = "outside")]
    Branch {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
        #[param("The branch's name.")]
        name: String,
        #[param("Names to text values only the person's keys read: {customer_id: \"cus_8121\"}.")]
        labels: Option<Map<String, Value>>,
        #[param("The new workspace's id (a UUID), so a retry asks for the same one.")]
        id: Option<String>,
    },
    /// Name a workspace
    #[effect(kind = "outside")]
    Rename {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
        #[param("Its new name.")]
        name: String,
    },
    /// Delete a workspace of yours
    #[effect(kind = "outside")]
    Delete {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
    },
    /// Keep a workspace of yours on this device
    #[effect(kind = "outside")]
    Enter {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
    },
    /// Stop keeping a workspace on this device
    #[effect(kind = "outside")]
    Leave {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
    },
    /// Join a workspace
    #[effect(kind = "outside")]
    Join {
        #[param("The invitation, as its link or text was handed to you.")]
        invite: Option<String>,
        #[param("The workspace to join through its open entry, by id; with `host`.")]
        workspace: Option<String>,
        #[param(
            "Who the workspace's host is and where it is dialled, as its directory publishes it \
             ({hostCert, address})."
        )]
        host: Option<Map<String, Value>>,
    },
    /// Run a workspace's commands unattended
    #[effect(kind = "outside")]
    Unattended {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
        #[param("true asks the person to allow it; false asks again from the next command.")]
        on: bool,
    },
    /// Take in a desktop's workspaces
    #[effect(kind = "outside")]
    Import {
        #[param(secret, "The key the desktop's key groups were sealed with, in hex.")]
        sealed_with: Option<String>,
        #[param("Each file of the desktop's key groups by name, its bytes in hex.")]
        groups: Option<Map<String, Value>>,
        #[param("The workspaces it took part in: {workspace, name, hint?}.")]
        workspaces: Option<Vec<Value>>,
        #[param(
            secret,
            "What its own workspace service administered, with the key it wrote under."
        )]
        service: Option<Map<String, Value>>,
    },
}

/// The account's keys: one asked for, and one minted for an agent of a workspace.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.account.keys")]
pub enum KeyAction {
    /// Ask the person for a new key
    #[effect(kind = "outside", returns = CreatedKey)]
    Create {
        #[param("What the key is called.")]
        name: String,
        #[param("The one workspace it reaches; all of the person's when omitted.")]
        workspace: Option<String>,
        #[param("Seconds until it expires.")]
        expires_in: Option<u64>,
        #[param("Whether it operates web pages in the workspace's browser.")]
        browser_pages: Option<bool>,
    },
    /// A key for one workspace, for an agent there
    #[effect(kind = "outside", returns = MintedKey)]
    Mint {
        #[param("The workspace, by id, as account/workspaces#list names it.")]
        workspace: String,
        #[param("What the key is called.")]
        name: Option<String>,
        #[param("Seconds until it expires.")]
        expires_in: Option<u64>,
    },
}

/// What your workspaces keep and ran, and your plan.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.account.usage")]
pub enum UsageAction {
    /// Read your usage
    #[effect(kind = "read")]
    Read,
}

/// The person's own model, asked one turn at a time.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.account.models")]
pub enum ModelAction {
    /// Ask your model one turn
    #[effect(kind = "outside")]
    Ask {
        #[param("The workspace whose model answers.")]
        workspace: String,
        #[param("The conversation, as the model service takes one.")]
        request: Map<String, Value>,
    },
}

/// The key's webhooks: the workspace's events posted to a URL.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.account.webhooks")]
pub enum WebhookAction {
    /// List this key's webhooks
    #[effect(kind = "read")]
    List {
        #[param("Only this workspace's, by id.")]
        workspace: Option<String>,
    },
    /// Post a workspace's events to a URL
    #[effect(kind = "outside", returns = CreatedWebhook)]
    Create {
        #[param("The workspace whose events are posted.")]
        workspace: String,
        #[param("Where each event is posted.")]
        url: String,
        #[param("The kinds to post (change, invocation, presence); all when omitted.")]
        events: Option<Vec<String>>,
        #[param("One resource's changes only, and what is under it.")]
        path: Option<String>,
    },
    /// Stop posting to a webhook of this key
    #[effect(kind = "outside")]
    Delete {
        #[param("The webhook, as account/webhooks#list names it.")]
        webhook: String,
    },
}

/// What `account/workspaces` reads as: its workspaces and the hosts a new one may be made on.
#[cfg(test)]
#[derive(tilo_resource::JsonSchema)]
#[allow(dead_code, reason = "read by the schema it describes, never in Rust")]
pub struct WorkspacesReading {
    pub workspaces: Vec<crate::server::Workspace>,
    pub hosts: Vec<HostRow>,
}

/// One host a workspace may be made on, as `account/workspaces#list` names it.
#[derive(serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema)]
pub struct HostRow {
    pub name: String,
    /// Whether a workspace is made on it when the caller names no host.
    pub default: bool,
}

/// What `account/keys#create` answers: the key, handed over once and never read back.
pub struct CreatedKey;

impl tilo_resource::JsonSchema for CreatedKey {
    fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"key": {"type": "string", "format": "password"}},
        })
    }
}

/// What `account/keys#mint` answers: the token, handed over once and never read back.
pub struct MintedKey;

impl tilo_resource::JsonSchema for MintedKey {
    fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"token": {"type": "string", "format": "password"}},
        })
    }
}

/// What `account/webhooks#create` answers: the webhook, and the key its deliveries are signed
/// with.
pub struct CreatedWebhook;

impl tilo_resource::JsonSchema for CreatedWebhook {
    fn json_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "workspace": {"type": "string"},
                "url": {"type": "string"},
                "events": {"type": "array", "items": {"type": "string"}},
                "path": {"type": "string"},
                "key": {"type": "string"},
            },
        })
    }
}

/// One of the account's actions, decoded once from a call's address and arguments.
pub(crate) enum Asked {
    Workspaces(WorkspaceAction),
    Keys(KeyAction),
    Usage(UsageAction),
    Models(ModelAction),
    Webhooks(WebhookAction),
}

impl Asked {
    /// The action `resource` and `action` name, with the call's `arguments`: `None` when the
    /// account publishes no such action there.
    pub(crate) fn decode(
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Self, ActionCallError>> {
        let asked = match resource {
            WORKSPACES => WorkspaceAction::decode(action, arguments).map(Self::Workspaces),
            KEYS => KeyAction::decode(action, arguments).map(Self::Keys),
            USAGE => UsageAction::decode(action, arguments).map(Self::Usage),
            MODELS => ModelAction::decode(action, arguments).map(Self::Models),
            WEBHOOKS => WebhookAction::decode(action, arguments).map(Self::Webhooks),
            _ => return None,
        };
        Some(asked.map_err(|error| {
            ActionCallError::invalid_request(format!("the call's arguments: {error}"))
        }))
    }

    /// The workspace it acts on, when it names one.
    pub(crate) fn workspace(&self) -> Option<&str> {
        match self {
            Self::Workspaces(
                WorkspaceAction::Branch { workspace, .. }
                | WorkspaceAction::Rename { workspace, .. }
                | WorkspaceAction::Delete { workspace }
                | WorkspaceAction::Enter { workspace }
                | WorkspaceAction::Leave { workspace }
                | WorkspaceAction::Unattended { workspace, .. },
            )
            | Self::Keys(KeyAction::Mint { workspace, .. })
            | Self::Models(ModelAction::Ask { workspace, .. })
            | Self::Webhooks(WebhookAction::Create { workspace, .. }) => Some(workspace),
            _ => None,
        }
    }
}

/// Every action the account publishes, with the resource it is published on and that resource's
/// label: each resource and the declaration of every action it publishes.
pub(crate) fn declarations() -> impl Iterator<Item = (&'static str, &'static str, EffectDecl)> {
    [
        (WORKSPACES, "Workspaces", WorkspaceAction::declarations()),
        (KEYS, "Keys", KeyAction::declarations()),
        (USAGE, "Usage", UsageAction::declarations()),
        (MODELS, "Models", ModelAction::declarations()),
        (WEBHOOKS, "Webhooks", WebhookAction::declarations()),
    ]
    .into_iter()
    .flat_map(|(resource, label, declared)| {
        declared
            .into_iter()
            .map(move |declaration| (resource, label, declaration))
    })
}

/// The catalogue row of `declaration`, published on `resource`, a resource labelled `label`.
pub(crate) fn row(resource: &str, label: &str, declaration: &EffectDecl) -> CatalogueRow {
    let kind = resource.replace('/', ".");
    let mut tags = Vec::new();
    // Reading the resource (`tilo_read account/workspaces`) lists it.
    if declaration.action_kind == ActionKind::Read {
        tags.push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
    }
    CatalogueRow {
        offered: None,
        provider: PROVIDER.into(),
        app_id: PROVIDER.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.clone(),
            label: label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, declaration.id),
            resource_kind: kind,
            resource_label: label.into(),
            action_label: declaration.label.read(),
            keywords: declaration
                .keywords
                .iter()
                .map(|word| (*word).to_owned())
                .collect(),
            icon: declaration.icon.map(str::to_owned),
            description: (!declaration.description.is_empty())
                .then(|| declaration.description.to_owned()),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: declaration.action_kind,
                risk: declaration.risk,
                tags,
                result: declaration
                    .result
                    .as_ref()
                    .map_or(ResultSpec::None, |result| ResultSpec::Json {
                        schema: (result.schema)(),
                    }),
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: declaration.params.clone(),
            view: None,
        }),
        has_children: resource == KEYS,
        visibility: None,
        surface: None,
    }
}

/// The account's surface, as `account-surface.json` carries it: one resource per vocabulary.
#[cfg(test)]
pub(crate) fn surface() -> Value {
    use tilo_resource::surface::reading_surface;

    json!({"resources": [
        reading_surface::<WorkspacesReading, WorkspaceAction>("account.workspaces", "Workspaces"),
        reading_surface::<Value, KeyAction>("account.keys", "Keys"),
        reading_surface::<Value, UsageAction>("account.usage", "Usage"),
        reading_surface::<Value, ModelAction>("account.models", "Models"),
        reading_surface::<Value, WebhookAction>("account.webhooks", "Webhooks"),
    ]})
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion"
    )]

    use super::*;

    /// `account-surface.json` is what this module declares, and every action decodes from its own
    /// id on the resource it is published on: the file another language writes its typed methods
    /// from and a tilolang view compiles against. `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_account_surface_file_is_what_this_module_publishes() {
        let text = tilo_surface_compile::canonical(&surface());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("account-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
        } else {
            assert_eq!(
                std::fs::read_to_string(&path).ok().as_deref(),
                Some(text.as_str()),
                "crates/core/tilo-api/account-surface.json is not what this module publishes; \
                 regenerate it with UPDATE_SNAPSHOTS=1"
            );
        }
        for (resource, _, declaration) in declarations() {
            assert!(
                Asked::decode(resource, declaration.id, &Map::new()).is_some(),
                "{resource}#{}",
                declaration.id
            );
        }
        assert!(!matches!(
            Asked::decode(WORKSPACES, "revoke", &Map::new()),
            Some(Ok(_))
        ));
    }
}
