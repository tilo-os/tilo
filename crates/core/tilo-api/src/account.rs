//! The account: a person's workspaces, their keys, what their workspaces
//! keep, and their model, published as one provider ([`Account`], app
//! [`PROVIDER`], reached by the path `account`) in every tree a key walks.
//! An agent, a program and a route of the API reach it as they reach any
//! action: by path, through the one walk of the tree.
//!
//! - `account/workspaces#list {labels?}`, `#create {name, labels?, id?}`,
//!   `#branch {workspace, name, labels?, id?}`, `#rename {workspace, name}`,
//!   `#delete {workspace}`, `#unattended {workspace, on}`, and on a computer's own Tilo service
//!   `#import {sealed_with?, groups?, workspaces?}`, which takes in once what its desktop kept;
//! - `account/keys#create {name, workspace?, expires_in?}`,
//!   `#mint {workspace, name?, expires_in?}`, and one resource per key,
//!   `account/keys/<grant>#revoke`. The keys are each workspace's own grants, each a record
//!   of [`tilo_actions::remote::KEY_GRANTS`] a listing of the workspace names; no action lists
//!   them;
//! - `account/usage#read`;
//! - `account/models#ask {workspace, request}`;
//! - `account/webhooks#list {workspace?}`, `#create {workspace, url, events?, path?}`,
//!   `#delete {webhook}`: the key's webhooks, records of the workspace that keeps them, posted
//!   by the device that keeps it.
//!
//! Each invocation is admitted as every action is ([`tilo_actions::policy::admit`]): a key for
//! one workspace reaches its own keys, its workspace's setting and its model, and nothing else of
//! the account; a key for all the person's workspaces reaches all of it.
//!
//! Making, branching, renaming and deleting a workspace are the server's own operations
//! ([`crate::server`]). Minting a key is a certificate the grant's own key signs for one
//! workspace ([`tilo_user_identity::prepare_minted_grant`]): narrower than the grant, ending no
//! later than it and within 90 days, and revoked with it. Creating a key asks the person, whose
//! device certifies it ([`crate::keys`]); the invocation waits for them. Deleting a workspace is
//! final, tells whoever is in it, and revokes every key that reached only it; the call is the
//! confirmation. Branching a workspace of the person's own makes a new one of theirs that starts
//! with everything the source holds but what is tied to other people.
//!
//! Turning a workspace's unattended runs on asks the person on their approving device, and the
//! invocation waits for them; off is done at once. On, a key for that workspace alone runs
//! commands in its Linux machine without the person being asked each time. A model asked is the
//! person's own, kept by the device that keeps the workspace: a server, or the computer that
//! keeps its person's account itself (`tilo.models`, `ask`). While it
//! writes, the invocation's newest item holds the answer so far.

use crate::grant_key::GrantKey;
use crate::server::{Connection, ServerError};
use crate::session::{Here, Provider, Reach};
use crate::store::{Grant, GrantKind, Scope};
use crate::{Server, now_ms};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use tilo_actions::policy::roles::governance::RENAME;
use tilo_actions::policy::{Answer, Asker, Invoked};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget, pending,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput,
    ActionParam, ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind,
    ParamType, ResultSpec, RiskLevel,
};
use tilo_invocations::Invocations;
use tilo_server_protocol::workspaces::{
    BRANCH_OWN_WORKSPACE, CREATE_OWN_WORKSPACE, CREATE_WEBHOOK, CreatedWorkspace, DELETE_WEBHOOK,
    DELETE_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_WEBHOOKS, REVOKE_KEY,
    RUN_UNATTENDED, WorkspaceLabels,
};
use tilo_session_actions::SessionActions;
use tilo_user_identity::AgentScope;

/// The workspace's model providers, as the service that keeps them publishes their actions.
mod model_providers {
    tilo_resource_macros::bindings!(
        provider = "tilo.models",
        surface = "../workspace-records/models-surface.json"
    );
}

/// The app the account is published as: `account` names it.
pub const PROVIDER: &str = "tilo.account";
/// The person's workspaces.
pub const WORKSPACES: &str = "account/workspaces";
/// The keys to them.
pub const KEYS: &str = "account/keys";
/// One of the keys, as a resource of its own: the key's name, revoked where it is listed.
const KEY_KIND: &str = "account.keys.key";
/// What the workspaces keep and ran.
pub const USAGE: &str = "account/usage";
/// The person's own model.
pub const MODELS: &str = "account/models";
/// The key's webhooks.
pub const WEBHOOKS: &str = "account/webhooks";
/// The revision every page of the account is at: what it publishes never changes.
const REVISION: &str = "account:1";
/// How many ended invocations are kept to be observed.
const KEPT: usize = 256;
/// How long turning a workspace's unattended runs on waits for the host before it is followed:
/// off is answered at once, on once the person decided, which takes longer.
const ANSWERING_MS: u32 = 1_000;

/// One of the account's actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ListWorkspaces,
    CreateWorkspace,
    BranchWorkspace,
    RenameWorkspace,
    DeleteWorkspace,
    EnterWorkspace,
    LeaveWorkspace,
    JoinWorkspace,
    Unattended,
    ImportWorkspaces,
    CreateKey,
    MintKey,
    RevokeKey,
    Usage,
    AskModel,
    ListWebhooks,
    CreateWebhook,
    DeleteWebhook,
}

/// An action as the account publishes it.
#[derive(Clone, Copy)]
struct Verb {
    resource: &'static str,
    id: &'static str,
    label: &'static str,
    kind: ActionKind,
    op: Op,
    params: fn() -> Vec<ActionParam>,
    /// The JSON schema of what it answers, where it answers a secret.
    result: Option<fn() -> Value>,
}

const fn verb(
    resource: &'static str,
    id: &'static str,
    label: &'static str,
    kind: ActionKind,
    op: Op,
    params: fn() -> Vec<ActionParam>,
) -> Verb {
    Verb {
        resource,
        id,
        label,
        kind,
        op,
        params,
        result: None,
    }
}

/// `verb`, declaring the schema of what it answers.
const fn answers(verb: Verb, schema: fn() -> Value) -> Verb {
    Verb {
        resource: verb.resource,
        id: verb.id,
        label: verb.label,
        kind: verb.kind,
        op: verb.op,
        params: verb.params,
        result: Some(schema),
    }
}

const VERBS: [Verb; 17] = [
    verb(
        WORKSPACES,
        "list",
        "Your workspaces, whether this device keeps each, and the hosts you can create on; with \
         labels, those labelled so",
        ActionKind::Read,
        Op::ListWorkspaces,
        || vec![labels_param()],
    ),
    verb(
        WORKSPACES,
        "create",
        "Create a workspace; the host may refuse (quota, payment)",
        ActionKind::Outside,
        Op::CreateWorkspace,
        || {
            vec![
                ActionParam::required("name", ParamType::Text, "The workspace's name."),
                labels_param(),
                id_param(),
            ]
        },
    ),
    verb(
        WORKSPACES,
        "branch",
        "Branch a workspace of yours: a new one of yours that starts as its copy and goes on apart",
        ActionKind::Outside,
        Op::BranchWorkspace,
        || {
            vec![
                workspace_param(),
                ActionParam::required("name", ParamType::Text, "The branch's name."),
                labels_param(),
                id_param(),
            ]
        },
    ),
    verb(
        WORKSPACES,
        "rename",
        "Name a workspace, for every list and device of it; its owners and moderators may",
        ActionKind::Outside,
        Op::RenameWorkspace,
        || {
            vec![
                workspace_param(),
                ActionParam::required("name", ParamType::Text, "Its new name."),
            ]
        },
    ),
    verb(
        WORKSPACES,
        "delete",
        "Delete a workspace of yours and everything in it, for good; its keys are revoked",
        ActionKind::Outside,
        Op::DeleteWorkspace,
        || vec![workspace_param()],
    ),
    verb(
        WORKSPACES,
        "enter",
        "Keep a workspace of yours on this device, synced with its host",
        ActionKind::Outside,
        Op::EnterWorkspace,
        || vec![workspace_param()],
    ),
    verb(
        WORKSPACES,
        "leave",
        "Stop keeping a workspace on this device; you stay in it",
        ActionKind::Outside,
        Op::LeaveWorkspace,
        || vec![workspace_param()],
    ),
    verb(
        WORKSPACES,
        "join",
        "Join a workspace, by the invitation that brings you in or through its open entry, and \
         keep it on this device",
        ActionKind::Outside,
        Op::JoinWorkspace,
        || {
            vec![
                ActionParam::optional(
                    "invite",
                    ParamType::Text,
                    "The invitation, as its link or text was handed to you.",
                ),
                ActionParam::optional(
                    "workspace",
                    ParamType::Text,
                    "The workspace to join through its open entry, by id; with `host`.",
                ),
                ActionParam::optional(
                    "host",
                    ParamType::Object,
                    "Who the workspace's host is and where it is dialled, as its directory \
                     publishes it ({hostCert, address}).",
                ),
            ]
        },
    ),
    verb(
        WORKSPACES,
        "unattended",
        "Let keys made for one workspace alone run commands in its machine without asking each \
         time (asks you first), or ask again",
        ActionKind::Outside,
        Op::Unattended,
        || {
            vec![
                workspace_param(),
                ActionParam::required(
                    "on",
                    ParamType::Boolean,
                    "true asks the person to allow it; false asks again from the next command.",
                ),
            ]
        },
    ),
    verb(
        WORKSPACES,
        "import",
        "Take in, once, the workspaces and keys this computer's desktop kept itself",
        ActionKind::Outside,
        Op::ImportWorkspaces,
        || {
            vec![
                ActionParam::optional(
                    "sealed_with",
                    ParamType::Secret,
                    "The key the desktop's key groups were sealed with, in hex.",
                ),
                ActionParam::optional(
                    "groups",
                    ParamType::Object,
                    "Each file of the desktop's key groups by name, its bytes in hex.",
                ),
                ActionParam::optional(
                    "workspaces",
                    ParamType::Array,
                    "The workspaces it took part in: {workspace, name, hint?}.",
                ),
                ActionParam::optional(
                    "founds_person_workspace",
                    ParamType::Boolean,
                    "Whether it was still to found its person's own workspace.",
                ),
                ActionParam::optional(
                    "service",
                    ParamType::Secret,
                    "What its own workspace service administered, with the key it wrote under.",
                ),
            ]
        },
    ),
    answers(
        verb(
            KEYS,
            "create",
            "Ask the person for a new key; it waits for them",
            ActionKind::Outside,
            Op::CreateKey,
            || {
                vec![
                    ActionParam::required("name", ParamType::Text, "What the key is called."),
                    ActionParam::optional(
                        "workspace",
                        ParamType::Text,
                        "The one workspace it reaches; all of the person's when omitted.",
                    )
                    .of_resource_kind("tilo.workspace"),
                    expires_in_param(),
                    ActionParam::optional(
                        "browser_pages",
                        ParamType::Boolean,
                        "Whether it operates web pages in the workspace's browser.",
                    ),
                ]
            },
        ),
        || secret_result("key"),
    ),
    answers(
        verb(
            KEYS,
            "mint",
            "A key for one workspace, for an agent there",
            ActionKind::Outside,
            Op::MintKey,
            || {
                vec![
                    workspace_param(),
                    ActionParam::optional("name", ParamType::Text, "What the key is called."),
                    expires_in_param(),
                ]
            },
        ),
        || secret_result("token"),
    ),
    verb(
        USAGE,
        "read",
        "What your workspaces keep and ran, and your plan",
        ActionKind::Read,
        Op::Usage,
        Vec::new,
    ),
    verb(
        MODELS,
        "ask",
        "Ask your own model one turn; the invocation's newest item holds its answer so far",
        ActionKind::Outside,
        Op::AskModel,
        || {
            vec![
                workspace_param(),
                ActionParam::required(
                    "request",
                    ParamType::Object,
                    "The conversation, as the model service takes one.",
                ),
            ]
        },
    ),
    verb(
        WEBHOOKS,
        "list",
        "This key's webhooks, with the key their deliveries are signed with; with workspace, that workspace's",
        ActionKind::Read,
        Op::ListWebhooks,
        || {
            vec![
                ActionParam::optional(
                    "workspace",
                    ParamType::Text,
                    "Only this workspace's, by id.",
                )
                .of_resource_kind("tilo.workspace"),
            ]
        },
    ),
    answers(
        verb(
            WEBHOOKS,
            "create",
            "Post a workspace's events to a URL, signed by the device that keeps it; answers the key it signs with",
            ActionKind::Outside,
            Op::CreateWebhook,
            || {
                vec![
                    workspace_param(),
                    ActionParam::required("url", ParamType::Text, "Where each event is posted."),
                    ActionParam::optional(
                        "events",
                        ParamType::Array,
                        "The kinds to post (change, invocation, presence); all when omitted.",
                    ),
                    ActionParam::optional(
                        "path",
                        ParamType::Text,
                        "One resource's changes only, and what is under it.",
                    ),
                ]
            },
        ),
        || {
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
        },
    ),
    verb(
        WEBHOOKS,
        "delete",
        "Stop posting to a webhook of this key",
        ActionKind::Outside,
        Op::DeleteWebhook,
        || {
            vec![ActionParam::required(
                "webhook",
                ParamType::Text,
                "The webhook, as account/webhooks#list names it.",
            )]
        },
    ),
];

fn workspace_param() -> ActionParam {
    ActionParam::required(
        "workspace",
        ParamType::Text,
        "The workspace, by id, as account/workspaces#list names it.",
    )
    .of_resource_kind("tilo.workspace")
}

/// The schema of a result that answers the secret `field`: a reader withholds it.
fn secret_result(field: &str) -> Value {
    json!({
        "type": "object",
        "properties": { field: {"type": "string", "format": "password"} },
    })
}

fn labels_param() -> ActionParam {
    ActionParam::optional(
        "labels",
        ParamType::Object,
        "Names to text values only the person's keys read: {customer_id: \"cus_8121\"}.",
    )
}

fn id_param() -> ActionParam {
    ActionParam::optional(
        "id",
        ParamType::Text,
        "The new workspace's id (a UUID), so a retry asks for the same one.",
    )
}

fn expires_in_param() -> ActionParam {
    ActionParam::optional(
        "expires_in",
        ParamType::Integer,
        "Seconds until it expires.",
    )
}

/// The label of the account's resource `resource`.
fn resource_label(resource: &str) -> &'static str {
    match resource {
        WORKSPACES => "Workspaces",
        KEYS => "Keys",
        USAGE => "Usage",
        WEBHOOKS => "Webhooks",
        _ => "Models",
    }
}

impl Verb {
    fn row(&self) -> CatalogueRow {
        let kind = self.resource.replace('/', ".");
        let label = resource_label(self.resource);
        let mut tags = Vec::new();
        // Reading the resource (`tilo_read account/workspaces`) lists it.
        if self.kind == ActionKind::Read {
            tags.push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
        }
        CatalogueRow {
            offered: None,
            provider: PROVIDER.into(),
            app_id: PROVIDER.into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: self.resource.into(),
                kind: kind.clone(),
                label: label.into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: ActionAddress::new(self.resource, self.id),
                resource_kind: kind,
                resource_label: label.into(),
                action_label: self.label.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: ExecutionLocus::ServerPreferred,
                kind: OperationKind::Effect,
                contract: ActionContract {
                    kind: self.kind,
                    risk: if self.kind == ActionKind::Read {
                        RiskLevel::None
                    } else {
                        RiskLevel::Privileged
                    },
                    tags,
                    result: self
                        .result
                        .map_or(ResultSpec::None, |schema| ResultSpec::Json {
                            schema: schema(),
                        }),
                    ..ActionContract::default()
                },
                input: InputSpec::None,
                params: (self.params)(),
                view: None,
            }),
            has_children: self.resource == KEYS,
            visibility: None,
            surface: None,
        }
    }
}

/// One key as a resource under `account/keys`, by its grant: what its revocation is asked on.
fn key_row(grant: &str) -> CatalogueRow {
    let name = grant;
    let id = format!("{KEYS}/{grant}");
    CatalogueRow {
        offered: None,
        provider: PROVIDER.into(),
        app_id: PROVIDER.into(),
        parent_resource: KEYS.into(),
        resource: CatalogueResource {
            id: id.clone(),
            kind: KEY_KIND.into(),
            label: name.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(id.as_str(), "revoke"),
            resource_kind: KEY_KIND.into(),
            resource_label: name.into(),
            action_label: "Revoke this key and every key it minted".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: ActionKind::Outside,
                risk: RiskLevel::Privileged,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The key `resource` addresses, when it addresses one: `account/keys/<grant>`.
fn key_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(KEYS)?
        .strip_prefix('/')
        .filter(|grant| !grant.is_empty())
}

/// The account's actions, as one key's session reaches them.
#[derive(Clone)]
pub struct Account(Arc<Inner>);

struct Inner {
    server: Weak<Server>,
    /// The key's grant as its session was opened with it: an API server that never saw the
    /// key approved holds it in no store until the workspace let it in.
    grant: Grant,
    reach: Reach,
    /// Who the account's calls on a workspace's host are by: the key's person at the key.
    caller: Caller,
    running: Mutex<Running>,
}

/// The invocations the account answers after it admitted them.
#[derive(Default)]
struct Running {
    by_id: HashMap<InvocationId, Run>,
    order: VecDeque<InvocationId>,
}

enum Run {
    /// The person is asked for a key: its poll secret.
    Key(String),
    /// It ended so.
    Ended(InvocationState),
}

/// What an action answered.
pub(crate) enum Outcome {
    Ok(Value),
    /// Asked of the person: the poll secret of the key they are asked for.
    Key(String),
    /// Asked of a workspace's host, which runs it on.
    Asked(SessionActions),
    /// Asked of a workspace's host, which holds it for the person to allow: the caller is
    /// answered that it waits for them, and the same invocation answers how they decided.
    Held(SessionActions),
    /// Run at a provider, recorded so its caller follows it there by its receipt.
    Ran(Box<InvocationReceipt>),
    Failed {
        code: &'static str,
        message: String,
        next: String,
    },
}

pub(crate) fn failed(
    code: &'static str,
    message: impl Into<String>,
    next: impl Into<String>,
) -> Outcome {
    Outcome::Failed {
        code,
        message: message.into(),
        next: next.into(),
    }
}

/// What `error` tells the caller.
pub(crate) fn refused(error: &ServerError) -> Outcome {
    match error {
        ServerError::NotFound => failed(
            "not_found",
            "no such workspace of yours",
            "account/workspaces#list names your workspaces",
        ),
        // The host's own reason stays in the log: the person reads what they
        // would do about it.
        ServerError::KeyRevoked => {
            failed("key_revoked", "this key was revoked", crate::APPROVE_AGAIN)
        }
        ServerError::Refused(_) | ServerError::RefusedWorkspace(_) => failed(
            "refused",
            error.to_string(),
            "ask the person whether their plan or workspace rules allow it, then try again",
        ),
        ServerError::Unavailable(_) => {
            failed("unavailable", error.to_string(), "try again shortly")
        }
        ServerError::AwaitingApproval(_) => failed(
            crate::v1::AWAITING_APPROVAL,
            error.to_string(),
            "tell the person to approve it in their Tilo; it is done once they allow it",
        ),
        ServerError::NotOnThisServer(_) => failed(
            tilo_actions::remote::NOT_ON_THIS_SERVER,
            error.to_string(),
            "ask the server's operator to declare the person's key an owner, or have someone \
             there send the person an invitation link",
        ),
        ServerError::NoAccountHere => failed(
            "not_permitted",
            error.to_string(),
            "make, branch and delete workspaces in Tilo",
        ),
        // Nothing that can read the workspace answered: the message says what
        // to do.
        ServerError::NoHolder => failed(
            "unavailable",
            error.to_string(),
            "open Tilo on a device that holds the workspace, then try again",
        ),
    }
}

/// The code of a branch that could not take everything of its source: it was
/// not made, and the message names the part that did not come.
pub(crate) const BRANCH_INCOMPLETE: &str = "branch_incomplete";

/// The answer to a branch the host could not make whole: none was made, and
/// the caller reads which part of the source did not come; why stays in the
/// log, as for every refusal. `None` for any other refusal.
pub(crate) fn not_branched(error: &ServerError) -> Option<Outcome> {
    let ServerError::RefusedWorkspace(
        tilo_server_protocol::workspaces::AdminRefusal::BranchNotMade { part },
    ) = error
    else {
        return None;
    };
    tracing::info!(%part, "the server made no branch");
    Some(failed(
        BRANCH_INCOMPLETE,
        format!("The branch wasn't made: {part} didn't come along. Nothing of it was kept."),
        "try the same call again in a little while; if the same part fails again, tell the person",
    ))
}

/// The failure `outcome` says, as an invocation ends with it.
fn failure(code: &str, message: String, next: &str) -> ActionFailure {
    ActionFailure {
        retryable: code == "unavailable" || code == BRANCH_INCOMPLETE,
        code: code.to_owned(),
        message,
        details: Some(json!({"next_steps": [next]})),
    }
}

pub(crate) fn grant_row(grant: &Grant) -> Value {
    json!({
        "grant": grant.id,
        "name": grant.client_name,
        "scope": grant.scope.name(),
        "kind": match grant.kind {
            GrantKind::Agent => "agent",
            GrantKind::Key => "key",
            GrantKind::Minted { .. } => "minted",
        },
        "created_at_ms": grant.created_ms,
        "created_by": grant.created_by,
        "browser_pages": grant.browser_pages,
        // To the minute: every call uses a key, and the keys' record changes only by what was
        // done to them, not by a reader reading it.
        "last_used_at_ms": grant
            .last_used_ms
            .map(|ms| ms.saturating_sub(ms % crate::store::LAST_USED_SAVE_MS)),
        "expires_at_ms": grant.expires_ms,
    })
}

impl Account {
    pub(crate) fn new(server: Weak<Server>, grant: Grant, reach: Reach, caller: Caller) -> Self {
        Self(Arc::new(Inner {
            server,
            grant,
            reach,
            caller,
            running: Mutex::default(),
        }))
    }

    /// The page `query` asks of the account, in the tree of `workspace`.
    fn page(query: &CatalogueQuery, workspace: &str) -> CataloguePage {
        if query
            .provider
            .as_deref()
            .is_some_and(|provider| provider != PROVIDER)
        {
            return CataloguePage {
                revision: REVISION.into(),
                exhausted: true,
                ..CataloguePage::default()
            };
        }
        let rows = VERBS.iter().map(Verb::row).collect();
        let mut page = CataloguePage::from_rows(REVISION.into(), rows, query);
        if query.provider.is_none() && query.cursor.is_empty() {
            page.providers.push(publisher(workspace));
        }
        page
    }

    /// The page `query` asks of the account, in the tree of `workspace`: the key it names when
    /// it asks for the exact action on one, the verbs otherwise. No page lists the keys: each
    /// workspace's listing names its own ([`tilo_actions::remote::KEY_GRANTS`]).
    fn ask(query: &CatalogueQuery, workspace: &str) -> CataloguePage {
        let Some(grant) = query
            .target
            .as_ref()
            .and_then(|target| key_of(&target.resource))
            .filter(|_| {
                query
                    .provider
                    .as_deref()
                    .is_none_or(|provider| provider == PROVIDER)
            })
        else {
            return Self::page(query, workspace);
        };
        let mut page = CataloguePage::from_rows(REVISION.into(), vec![key_row(grant)], query);
        if query.provider.is_none() && query.cursor.is_empty() {
            page.providers.push(publisher(workspace));
        }
        page
    }

    fn keep(&self, id: InvocationId, run: Run) {
        let mut running = crate::lock(&self.0.running);
        if running.by_id.insert(id, run).is_none() {
            running.order.push_back(id);
        }
        while running.order.len() > KEPT {
            if let Some(oldest) = running.order.pop_front() {
                running.by_id.remove(&oldest);
            }
        }
        drop(running);
    }

    /// Answered the next time the workspaces the account lists may have changed: what a watch of
    /// `account/workspaces` waits on. `None` where nothing tells.
    fn changed(&self) -> Option<Pending<Result<(), ActionCallError>>> {
        let Reach::Here(here, _) = &self.0.reach else {
            return None;
        };
        Some(here.workspaces_changed()?.map(Ok))
    }

    /// Whether the account answers the invocation `id` for `caller`: one of its own, or one it
    /// handed on to a workspace's host for them.
    fn knows(&self, caller: &Caller, id: &InvocationId) -> bool {
        crate::lock(&self.0.running).by_id.contains_key(id)
            || self
                .handed_on(|invocations| invocations.get(caller, id))
                .is_some()
    }

    /// What the device's record of the invocations handed on answers, while the server runs.
    fn handed_on<T>(&self, read: impl FnOnce(&Invocations) -> Option<T>) -> Option<T> {
        read(&self.0.server.upgrade()?.invocations)
    }

    /// Run `request` for `caller`: its receipt.
    async fn run(&self, caller: Caller, request: InvokeRequest) -> InvocationReceipt {
        let id = request.invocation;
        // A call handed on to a workspace's host is followed there, through the device's record.
        let hand_on = |actions: SessionActions| {
            if let Some(server) = self.0.server.upgrade() {
                server.invocations.record(
                    &caller,
                    &caller,
                    &request,
                    None,
                    (Arc::new(actions), self.0.caller.clone()),
                );
            }
        };
        let args = match &request.argument {
            InvokeArgument::Json(arguments) => Value::Object(arguments.clone()),
            _ => json!({}),
        };
        let address = &request.target.address;
        let outcome = if let Some(verb) = VERBS
            .iter()
            .find(|verb| (verb.resource, verb.id) == (&*address.resource, &*address.action))
        {
            self.answer(verb.op, id, &args).await
        } else {
            // A key is a resource of its own, revoked where it is listed.
            let Some(grant) = key_of(&address.resource).filter(|_| address.action == "revoke")
            else {
                return InvocationReceipt {
                    invocation: id,
                    status: ReceiptStatus::Rejected {
                        error: ActionCallError::UnknownAction,
                    },
                };
            };
            self.answer(Op::RevokeKey, id, &json!({"grant": grant}))
                .await
        };
        let status = match outcome {
            Outcome::Ok(value) => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    ..ActionOutput::default()
                },
            },
            Outcome::Key(poll) => {
                self.keep(id, Run::Key(poll));
                ReceiptStatus::Accepted
            }
            Outcome::Asked(actions) => {
                hand_on(actions);
                ReceiptStatus::Accepted
            }
            Outcome::Held(actions) => {
                hand_on(actions);
                ReceiptStatus::Rejected {
                    error: ActionCallError::confirmation_required(),
                }
            }
            Outcome::Ran(receipt) => receipt.status,
            Outcome::Failed {
                code,
                message,
                next,
            } => ReceiptStatus::Failed {
                error: failure(code, message, &next),
            },
        };
        let ended = match &status {
            ReceiptStatus::Completed { output } => Some(InvocationUpdate::Succeeded {
                invocation: id,
                output: output.clone(),
            }),
            ReceiptStatus::Failed { error } => Some(InvocationUpdate::Failed {
                invocation: id,
                error: error.clone(),
            }),
            _ => None,
        };
        if let Some(update) = ended {
            self.keep(id, Run::Ended(InvocationState::new(id, update)));
        }
        InvocationReceipt {
            invocation: id,
            status,
        }
    }

    /// Admit `op` with `args` for this key, and run it.
    async fn answer(&self, op: Op, id: InvocationId, args: &Value) -> Outcome {
        let Some(server) = self.0.server.upgrade() else {
            return failed("unavailable", "the server is stopping", "try again shortly");
        };
        if server.store.revoked(&self.0.grant.id) {
            return failed("key_revoked", "this key was revoked", crate::APPROVE_AGAIN);
        }
        let grant = server
            .store
            .grant(&self.0.grant.id)
            .unwrap_or_else(|| self.0.grant.clone());
        if tilo_actions::policy::admit(
            Invoked {
                answer: answer_for(&grant, op, args),
                attended: false,
                outside: false,
                answered_in_advance: false,
                resource: None,
            },
            Asker {
                reachable: false,
                delegated: false,
            },
        )
        .is_err()
        {
            return failed(
                "not_permitted",
                "this key only works in one workspace",
                "to create or list workspaces, ask the person for a key for all their workspaces",
            );
        }
        let key = match &self.0.reach {
            Reach::Forwarded(key) => Arc::clone(key),
            // What a desktop kept itself is taken in by the device it is on, never by a server.
            Reach::Here(here, _) if op == Op::ImportWorkspaces => {
                return match here.import(&self.0.caller, args.clone()).await {
                    Ok(imported) => Outcome::Ok(imported),
                    Err(error) => failed(
                        "refused",
                        error,
                        "the desktop keeps its workspaces and asks again on its next start",
                    ),
                };
            }
            Reach::Here(here, key) => {
                // A device that serves the person's account itself answers their calls here, as
                // the key they were made with.
                if let Some(outcome) = self.answer_here(here.as_ref(), &grant, op, id, args).await {
                    return outcome;
                }
                return here_answers(here.as_ref(), key.cert(), &server, op, args).await;
            }
        };
        match op {
            Op::ListWorkspaces => workspaces(&key, &server, args).await,
            Op::EnterWorkspace | Op::LeaveWorkspace | Op::JoinWorkspace => failed(
                "not_permitted",
                "only one of the person's devices keeps workspaces",
                "ask it on the device that is to keep the workspace",
            ),
            Op::CreateWorkspace | Op::BranchWorkspace => {
                let (action, arguments) = match server_operation(op, args) {
                    Ok(asked) => asked,
                    Err(outcome) => return outcome,
                };
                match key.server().make(&key, action, arguments).await {
                    Ok(made) => Outcome::Ok(json!({"workspace": made})),
                    Err(error) => not_branched(&error).unwrap_or_else(|| refused(&error)),
                }
            }
            Op::RenameWorkspace => rename_workspace(&key, args).await,
            Op::DeleteWorkspace => server.delete_workspace(&grant, &key, args).await,
            Op::Unattended => self.unattended(&key, id, args).await,
            Op::CreateKey => server.create_key(args),
            Op::MintKey => server.mint(&grant, &key, args).await,
            Op::RevokeKey => server.revoke_key(&grant, &key, args).await,
            Op::Usage => match key.server().account(&key).await {
                Ok(account) => Outcome::Ok(account),
                Err(error) => refused(&error),
            },
            Op::AskModel => self.ask_model(&key, id, args).await,
            Op::ListWebhooks | Op::CreateWebhook | Op::DeleteWebhook => match &self.0.reach {
                Reach::Forwarded(connection) => forwarded_webhooks(connection, op, args).await,
                Reach::Here(..) => failed(
                    "unavailable",
                    "a webhook is kept by the device that keeps its workspace",
                    "ask it on the device that keeps the workspace",
                ),
            },
            Op::ImportWorkspaces => failed(
                "unavailable",
                "a server takes in no desktop's workspaces",
                "run this on the computer whose desktop kept them",
            ),
        }
    }

    /// What a device that serves its person's account itself answers `op` with `args`, as the
    /// key `grant` was made with: `None` for what its desktop keeps, leaves, joins or takes in,
    /// which [`here_answers`] answers.
    async fn answer_here(
        &self,
        here: &dyn crate::session::Here,
        grant: &Grant,
        op: Op,
        id: InvocationId,
        args: &Value,
    ) -> Option<Outcome> {
        if matches!(
            op,
            Op::EnterWorkspace | Op::LeaveWorkspace | Op::JoinWorkspace | Op::ImportWorkspaces
        ) {
            return None;
        }
        let host = here.account_here()?;
        let Some(key) = grant.key() else {
            return Some(refused(&ServerError::NoAccountHere));
        };
        Some(
            self.serve_here(here, host.as_ref(), key, grant, op, id, args)
                .await,
        )
    }

    /// Admit `op` with `args` for `key`, a device's own account served by its daemon.
    #[allow(
        clippy::too_many_lines,
        reason = "one match over the account's actions, each a few lines of arguments and an answer"
    )]
    async fn serve_here(
        &self,
        here: &dyn crate::session::Here,
        host: &dyn crate::session::AccountHere,
        key: GrantKey,
        grant: &Grant,
        op: Op,
        id: InvocationId,
        args: &Value,
    ) -> Outcome {
        let Some(server) = self.0.server.upgrade() else {
            return failed("unavailable", "the device is stopping", "try again shortly");
        };
        match op {
            Op::EnterWorkspace | Op::LeaveWorkspace | Op::JoinWorkspace | Op::ImportWorkspaces => {
                failed(
                    "unavailable",
                    "the desktop that keeps the workspace answers this",
                    "ask it on that device",
                )
            }
            Op::ListWorkspaces => Self::here_workspaces(host, &key, grant, args),
            Op::CreateWorkspace => {
                let Some(name) = field(args, "name") else {
                    return failed(
                        "invalid_arguments",
                        "`name` is required",
                        "pass {name: \"Applicant: Jane Doe\"}",
                    );
                };
                let (id, labels) = match (chosen_id(args), labels_of(args)) {
                    (Ok(id), Ok(labels)) => (id, labels),
                    (Err(refused), _) | (_, Err(refused)) => return refused,
                };
                match host.create(key, name.to_owned(), labels, id).await {
                    Ok(workspace) => Outcome::Ok(json!({"workspace": workspace})),
                    Err(error) => refused(&error),
                }
            }
            Op::BranchWorkspace => {
                let (Some(source), Some(name)) = (field(args, "workspace"), field(args, "name"))
                else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` and `name` are required",
                        "pass {workspace: \"<id>\", name}; account/workspaces#list names them",
                    );
                };
                let (id, labels) = match (chosen_id(args), labels_of(args)) {
                    (Ok(id), Ok(labels)) => (id, labels),
                    (Err(refused), _) | (_, Err(refused)) => return refused,
                };
                match host
                    .branch(key, source.to_owned(), name.to_owned(), labels, id)
                    .await
                {
                    Ok(branch) => Outcome::Ok(json!({"workspace": branch})),
                    Err(error) => not_branched(&error).unwrap_or_else(|| refused(&error)),
                }
            }
            Op::RenameWorkspace => {
                let (Some(workspace), Some(name)) = (field(args, "workspace"), field(args, "name"))
                else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` and `name` are required",
                        "pass {workspace: \"<id>\", name: \"Acme\"}; account/workspaces#list names them",
                    );
                };
                match host
                    .rename(key, workspace.to_owned(), name.to_owned())
                    .await
                {
                    Ok(workspace) => Outcome::Ok(json!({"workspace": workspace})),
                    Err(error) => refused(&error),
                }
            }
            Op::DeleteWorkspace => {
                let Some(workspace) = field(args, "workspace") else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` is required",
                        "pass {workspace: \"<id>\"}; account/workspaces#list names them",
                    );
                };
                match host.delete(key, workspace.to_owned()).await {
                    Ok(()) => {
                        // Every key that reached only that workspace goes with it.
                        let revoked = server.revoke_keys_for(&grant.person, workspace).await;
                        Outcome::Ok(json!({"deleted": workspace, "revoked_keys": revoked}))
                    }
                    Err(error) => refused(&error),
                }
            }
            Op::Unattended => {
                let (Some(workspace), Some(on)) = (
                    field(args, "workspace"),
                    args.get("on").and_then(Value::as_bool),
                ) else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` and `on` are required",
                        "pass {workspace, on: true} to ask the person, {workspace, on: false} to turn it off",
                    );
                };
                match host.unattended(key, workspace.to_owned(), on).await {
                    Ok(on) => Outcome::Ok(json!({"workspace": workspace, "unattended": on})),
                    Err(error) => refused(&error),
                }
            }
            Op::CreateKey => failed(
                "unavailable",
                "a key is asked for from the device that answers for the account",
                "create the key with account/keys#mint, or make one in Tilo",
            ),
            Op::MintKey => Self::here_mint(here, host, &key, grant, &server, args).await,
            Op::RevokeKey => self.here_revoke(host, &key, grant, &server, args).await,
            Op::Usage => failed(
                "unavailable",
                "a device keeps no plan or usage of its own to read",
                "read the account on the server that keeps the workspaces",
            ),
            Op::AskModel => {
                let (Some(workspace), Some(Value::Object(request))) =
                    (field(args, "workspace"), args.get("request"))
                else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` and `request` are required",
                        "pass {workspace, request: {messages, …}}",
                    );
                };
                let asked = InvokeRequest {
                    invocation: id,
                    target: model_providers::models::ask(),
                    argument: InvokeArgument::Json(request.clone()),
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                    signed: None,
                };
                // The workspace's model is asked where the workspace is kept, which is here.
                let Some((receipt, answers, runs_as)) =
                    host.ask(key, workspace.to_owned(), asked.clone()).await
                else {
                    return failed(
                        "unavailable",
                        "a model is asked on the device that keeps it",
                        "open Tilo on a device that keeps the workspace's model, then try again",
                    );
                };
                // Recorded as the device's own, so the caller follows the answer where it runs.
                if let Some(server) = self.0.server.upgrade() {
                    server.invocations.record(
                        &self.0.caller,
                        &runs_as,
                        &asked,
                        Some(workspace),
                        (answers, runs_as.clone()),
                    );
                }
                Outcome::Ran(Box::new(receipt))
            }
            Op::ListWebhooks => {
                let workspace = field(args, "workspace").map(str::to_owned);
                match host.webhooks(key, workspace).await {
                    Ok(listed) => Outcome::Ok(listed),
                    Err(error) => refused(&error),
                }
            }
            Op::CreateWebhook => {
                let (Some(workspace), Some(url)) = (field(args, "workspace"), field(args, "url"))
                else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` and `url` are required",
                        "pass {workspace, url, events?, path?}; account/workspaces#list names \
                         the workspaces",
                    );
                };
                if let Some(why) = tilo_net::address::webhook_refusal(url) {
                    return failed(
                        "invalid_arguments",
                        why,
                        "name an https URL on the public internet, or an address of this computer",
                    );
                }
                let events: Vec<String> = args
                    .get("events")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let path = field(args, "path").map(str::to_owned);
                let (workspace, url) = (workspace.to_owned(), url.to_owned());
                match host.create_webhook(key, workspace, url, events, path).await {
                    Ok(made) => Outcome::Ok(made),
                    Err(error) => refused(&error),
                }
            }
            Op::DeleteWebhook => {
                let Some(webhook) = field(args, "webhook") else {
                    return failed(
                        "invalid_arguments",
                        "`webhook` is required",
                        "account/webhooks#list names them",
                    );
                };
                let webhook = webhook.to_owned();
                match host.delete_webhook(key, webhook).await {
                    Ok(deleted) => Outcome::Ok(deleted),
                    Err(error) => refused(&error),
                }
            }
        }
    }

    /// The workspaces `key` reaches here, ones labelled with every one of `labels` `args` names.
    fn here_workspaces(
        host: &dyn crate::session::AccountHere,
        key: &GrantKey,
        grant: &Grant,
        args: &Value,
    ) -> Outcome {
        let labels = match labels_of(args) {
            Ok(labels) => labels,
            Err(refused) => return refused,
        };
        let mut workspaces = host.workspaces(key);
        workspaces.retain(|workspace| labelled(workspace, &labels));
        // A key for one workspace reaches that one, never its neighbours.
        if let Scope::Workspace(own) = &grant.scope {
            workspaces.retain(|workspace| &workspace.id == own);
        }
        Outcome::Ok(json!({"workspaces": workspaces, "hosts": []}))
    }

    /// Mint a key reaching one workspace, for an agent to act there, from the key that asks.
    async fn here_mint(
        here: &dyn crate::session::Here,
        host: &dyn crate::session::AccountHere,
        key: &GrantKey,
        grant: &Grant,
        server: &Server,
        args: &Value,
    ) -> Outcome {
        let Some(workspace) = args.get("workspace").and_then(Value::as_str) else {
            return failed(
                "invalid_arguments",
                "`workspace` is required",
                "account/workspaces#list names them",
            );
        };
        let Some(AgentScope::Workspace(id)) = AgentScope::parse(workspace) else {
            return failed(
                "invalid_arguments",
                "`workspace` is a workspace's id",
                "account/workspaces#list names them",
            );
        };
        if !host
            .workspaces(key)
            .iter()
            .any(|reached| reached.id == workspace)
        {
            return refused(&ServerError::NotFound);
        }
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(
                || format!("{}: {workspace}", grant.client_name),
                str::to_owned,
            );
        let name: String = name
            .chars()
            .filter(|character| !character.is_control())
            .take(crate::keys::MAX_NAME_CHARS)
            .collect();
        let now = now_ms();
        let minted = match mint_key(
            key,
            id,
            &name,
            args.get("expires_in").and_then(Value::as_u64),
            now,
        ) {
            Ok(minted) => minted,
            Err(outcome) => return outcome,
        };
        let kind = GrantKind::Minted {
            parent: grant.id.clone(),
        };
        let Ok((minted, token)) = server.store.resource_key(&minted, &name, kind, None, now) else {
            return failed(
                "unavailable",
                "the key could not be kept",
                "try again shortly",
            );
        };
        // The workspace keeps the new key's certificate before anyone holds it, so its first
        // read is answered as that key's own.
        if let Some(key) = minted.key()
            && let Err(error) = here.enter(workspace, &key).await
        {
            tracing::warn!(%error, workspace, grant = %minted.id, "a minted key is not recorded in its workspace yet");
        }
        Outcome::Ok(json!({
            "grant": minted.id,
            "name": minted.client_name,
            "token": token,
            "workspace": workspace,
            "mcp_url": format!("{}/w/{workspace}/mcp", server.config.issuer),
            "expires_at_ms": minted.expires_ms,
        }))
    }

    /// Revoke the key `args` names, and have the workspaces that keep it record the revocation.
    async fn here_revoke(
        &self,
        host: &dyn crate::session::AccountHere,
        key: &GrantKey,
        grant: &Grant,
        server: &Server,
        args: &Value,
    ) -> Outcome {
        let Some(id) = field(args, "grant") else {
            return failed(
                "invalid_arguments",
                "`grant` is required",
                "tilo_ls record:tilo.grants lists them in each workspace",
            );
        };
        let held_here = server
            .store
            .grant(id)
            .filter(|found| found.person == grant.person);
        let minted = server
            .store
            .grant(id)
            .is_some_and(|found| found.parent() == Some(grant.id.as_str()));
        if grant.scope == Scope::Account && persons_own(grant) && id != grant.id && !minted {
            // Each workspace that admitted the key keeps its grant, and records its revocation;
            // one that never admitted it has nothing to record.
            let mut recorded = false;
            for workspace in host.workspaces(key) {
                match host
                    .revoke_key(key.clone(), workspace.id.clone(), id.to_owned())
                    .await
                {
                    Ok(changed) => recorded |= changed,
                    Err(error) => {
                        tracing::warn!(%error, workspace = %workspace.id, "a workspace did not keep a key's revocation");
                    }
                }
            }
            if held_here.is_none() && !recorded {
                return failed(
                    "not_found",
                    "no such key of yours",
                    "tilo_ls record:tilo.grants lists them in each workspace",
                );
            }
            if held_here.is_some()
                && let Err(error) = server.revoke_grant(&grant.person, id).await
            {
                tracing::warn!(%error, grant = id, "a key's revocation was not kept");
                return failed(
                    "unavailable",
                    "revocation is unavailable",
                    "try again shortly",
                );
            }
            return Outcome::Ok(json!({"revoked": id}));
        }
        let own = id == grant.id || minted;
        if !own {
            return failed(
                "not_found",
                "no such key of this key",
                "tilo_ls record:tilo.grants lists them in each workspace",
            );
        }
        let workspace = match server
            .store
            .grant(id)
            .map_or_else(|| grant.scope.clone(), |found| found.scope)
        {
            Scope::Workspace(workspace) => Some(workspace),
            Scope::Account => None,
        };
        match server.revoke_grant(&grant.person, id).await {
            Ok(_) => {
                if let Some(workspace) = workspace
                    && let Err(error) = host.revoke_key(key.clone(), workspace, id.to_owned()).await
                {
                    tracing::warn!(%error, "a workspace did not record a key's revocation");
                }
                Outcome::Ok(json!({"revoked": id}))
            }
            Err(_) => failed(
                "unavailable",
                "revocation is unavailable",
                "try again shortly",
            ),
        }
    }

    /// Ask the workspace's host to turn its unattended runs on or off as `args` says
    /// (`{workspace, on}`), under the invocation `id`.
    async fn unattended(&self, key: &Connection, id: InvocationId, args: &Value) -> Outcome {
        let (Some(workspace), Some(on)) = (
            field(args, "workspace"),
            args.get("on").and_then(Value::as_bool),
        ) else {
            return failed(
                "invalid_arguments",
                "`workspace` and `on` are required",
                "pass {workspace, on: true} to ask the person, {workspace, on: false} to turn it off",
            );
        };
        let mut arguments = Map::new();
        arguments.insert("on".into(), json!(on));
        let target = RemoteActionTarget::workspace_governance(workspace, RUN_UNATTENDED);
        let refusal = |why: String| {
            failed(
                "not_permitted",
                format!("the workspace refused: {why}"),
                "only the workspace's owner, from a key of theirs reaching it, has it; \
                 turning it on waits for them to allow it",
            )
        };
        match self
            .ask_host(key, workspace, id, target, arguments, Some(ANSWERING_MS))
            .await
        {
            Ok(Outcome::Failed { message, .. }) => refusal(message),
            Ok(Outcome::Asked(actions)) => Outcome::Held(actions),
            Ok(asked) => asked,
            Err(error) => refusal(error.to_string()),
        }
    }

    /// Ask the person's model in `args`' workspace one turn of `args`' request, under the
    /// invocation `id`.
    async fn ask_model(&self, key: &Connection, id: InvocationId, args: &Value) -> Outcome {
        let (Some(workspace), Some(Value::Object(request))) =
            (field(args, "workspace"), args.get("request"))
        else {
            return failed(
                "invalid_arguments",
                "`workspace` and `request` are required",
                "pass {workspace, request: {messages, …}}",
            );
        };
        let target = model_providers::models::ask();
        match self
            .ask_host(key, workspace, id, target, request.clone(), None)
            .await
        {
            Ok(asked) => asked,
            Err(error) => failed("refused", error.to_string(), "try again shortly"),
        }
    }

    /// Invoke `target` with `arguments` on `workspace`'s host as the key's person, under `id`:
    /// done, followed there, or failed.
    async fn ask_host(
        &self,
        key: &Connection,
        workspace: &str,
        id: InvocationId,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
        wait_ms: Option<u32>,
    ) -> Result<Outcome, ActionCallError> {
        let entered = match key.server().enter(key, workspace).await {
            Ok(entered) => entered,
            Err(error) => return Ok(refused(&error)),
        };
        let actions = key.device.actions(entered);
        let request = InvokeRequest {
            invocation: id,
            target,
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms,
            appearance: None,
            signed: None,
        };
        let receipt = actions
            .invoke(&self.0.caller, request)
            .await
            .ok_or(ActionCallError::Unanswered)??;
        Ok(match receipt.status {
            ReceiptStatus::Completed { output } => Outcome::Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Accepted => Outcome::Asked(actions),
            ReceiptStatus::Failed { error } => failed("refused", error.message, "try again"),
            ReceiptStatus::Rejected { error } => return Err(error),
            ReceiptStatus::Cancelled { .. } => {
                failed("unavailable", "the workspace stopped it", "try again")
            }
        })
    }

    /// How the key the person was asked for under `poll` went, as invocation `id`.
    fn key_state(server: &Server, id: InvocationId, poll: &str) -> InvocationState {
        let ended = |code: &str, message: &str, next: &str| InvocationUpdate::Failed {
            invocation: id,
            error: failure(code, message.to_owned(), next),
        };
        let update = match server.key_answer(poll) {
            crate::keys::Answer::Approved { token, grant } => InvocationUpdate::Succeeded {
                invocation: id,
                output: ActionOutput {
                    value: Some(json!({
                        "key": token,
                        "grant": grant.id,
                        "name": grant.client_name,
                        "scope": grant.scope.name(),
                        "expires_at_ms": grant.expires_ms,
                    })),
                    ..ActionOutput::default()
                },
            },
            crate::keys::Answer::Declined => ended(
                "declined",
                "the person declined the key",
                "ask again only if they want it",
            ),
            crate::keys::Answer::Waiting { ask } => InvocationUpdate::Progress {
                invocation: id,
                sequence: 0,
                phase: "waiting".into(),
                completed: None,
                total: None,
                message: Some(format!("the person approves it at {ask}")),
            },
            crate::keys::Answer::Expired => ended(
                "expired",
                "nothing waits under this id: it expired, or the key was already answered",
                "ask again with account/keys#create",
            ),
        };
        InvocationState::new(id, update)
    }
}

/// The account, as the tree of `workspace` lists its publishers.
fn publisher(workspace: &str) -> CatalogueProvider {
    CatalogueProvider {
        provider: PROVIDER.into(),
        app_id: PROVIDER.into(),
        workspace: workspace.to_owned(),
        document_id: String::new(),
        browsable: true,
    }
}

/// What the grants answer `grant` for `op` with `args`: a key for all the person's workspaces
/// reaches all of the account; a key for one reaches its own keys, and its workspace's setting
/// and model.
fn answer_for(grant: &Grant, op: Op, args: &Value) -> Answer {
    // A minted key is a workspace's agent, narrower than the key that made it: what only the
    // person does, it never does.
    if matches!(grant.kind, GrantKind::Minted { .. })
        && matches!(
            op,
            Op::CreateWorkspace
                | Op::BranchWorkspace
                | Op::RenameWorkspace
                | Op::DeleteWorkspace
                | Op::Unattended
                | Op::CreateKey
                | Op::MintKey
        )
    {
        return Answer::Deny;
    }
    match &grant.scope {
        Scope::Account => Answer::Allow,
        Scope::Workspace(own) => match op {
            // Its list is the one workspace it reaches, as the server lists it for the key.
            Op::ListWorkspaces | Op::RevokeKey | Op::ListWebhooks | Op::DeleteWebhook => {
                Answer::Allow
            }
            Op::Unattended | Op::AskModel | Op::CreateWebhook
                if field(args, "workspace") == Some(own.as_str()) =>
            {
                Answer::Allow
            }
            _ => Answer::Deny,
        },
    }
}

impl ActionProvider for Account {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(Self::ask(&query, ROOT_TREE)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (done, answer) = pending();
        let account = self.clone();
        let caller = caller.clone();
        // An account answers from the person's own workspace's copy, which the service that
        // holds it reads on the runtime this device's programs run on: answered on a thread of
        // its own, as a record read is, so that read never waits on the runtime it runs on.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let invocation = request.invocation;
            let spawned = std::thread::Builder::new()
                .name("tilo-account".into())
                .spawn(move || {
                    let receipt = tilo_rt::app()
                        .block_on(account.run(caller, request))
                        .unwrap_or_else(|error| InvocationReceipt {
                            invocation,
                            status: ReceiptStatus::Rejected {
                                error: ActionCallError::Unavailable {
                                    reason: error.to_string(),
                                },
                            },
                        });
                    done.complete(Ok(receipt));
                });
            if let Err(error) = spawned {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: format!("the account could not answer: {error}"),
                }));
            }
        }
        #[cfg(target_arch = "wasm32")]
        tokio::spawn(async move {
            done.complete(Ok(account.run(caller, request).await));
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if let Some(state) = self.handed_on(|invocations| invocations.observe(caller, id)) {
            return Some(state);
        }
        let running = crate::lock(&self.0.running);
        match running.by_id.get(id)? {
            Run::Ended(state) => Some(state.clone()),
            Run::Key(poll) => {
                let poll = poll.clone();
                drop(running);
                let server = self.0.server.upgrade()?;
                let state = Self::key_state(&server, *id, &poll);
                if state.update.is_terminal() {
                    self.keep(*id, Run::Ended(state.clone()));
                }
                Some(state)
            }
        }
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.handed_on(|invocations| invocations.next_change(caller, id))
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        // This API reads no grants of its own, so the resource an invocation was made from gives
        // nobody here a say: only its caller, or the person its agent works for, stops it.
        let no_origin_grants =
            |_: &Caller, _: &tilo_action_services::workspace_invocations::Origin| false;
        if let Some(stopping) =
            self.handed_on(|invocations| invocations.cancel(caller, id, &no_origin_grants))
        {
            return stopping;
        }
        if crate::lock(&self.0.running).by_id.contains_key(id) {
            return Pending::ready(Err(ActionCallError::NotCancellable {
                reason: "it is not running".into(),
            }));
        }
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// The workspace the tree of a session that names none is the tree of: the account's own.
pub const ROOT_TREE: &str = "account";

/// A workspace's actions with the account's beside them: the tree a key's session walks. With
/// no workspace, the account's alone.
pub(crate) struct Joined {
    workspace: String,
    actions: Option<Provider>,
    account: Account,
}

impl Joined {
    pub(crate) fn provider(
        workspace: &str,
        actions: Option<Provider>,
        account: Account,
    ) -> Provider {
        Arc::new(Self {
            workspace: workspace.to_owned(),
            actions,
            account,
        })
    }

    fn is_account(target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
    }
}

impl ActionProvider for Joined {
    fn retire_execution(&self, caller: &Caller) {
        match &self.actions {
            Some(actions) => actions.retire_execution(caller),
            None => caller.retire_execution(),
        }
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let Some(actions) = self
            .actions
            .as_ref()
            .filter(|_| query.provider.as_deref() != Some(PROVIDER))
        else {
            return Pending::ready(Ok(Account::ask(&query, &self.workspace)));
        };
        if query.provider.is_some() || !query.cursor.is_empty() {
            return actions.query(caller, query);
        }
        let account = publisher(&self.workspace);
        actions.query(caller, query).map(move |answer| {
            answer.map(|mut page| {
                page.providers.push(account);
                page
            })
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match &self.actions {
            Some(actions) if !Self::is_account(&request.target) => actions.invoke(caller, request),
            _ => self.account.invoke(caller, request),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if self.account.knows(caller, id) {
            return self.account.observe(caller, id);
        }
        self.actions.as_ref()?.observe(caller, id)
    }

    fn observed(&self, caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        if let Some(actions) = &self.actions {
            actions.observed(caller, id, state);
        }
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if self.account.knows(caller, id) {
            return self.account.next_change(caller, id);
        }
        self.actions.as_ref()?.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.actions.as_ref()?.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.actions.as_ref()?.session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        match &self.actions {
            Some(actions) => actions.changed(caller, scope),
            // The account's own tree: its records are the person's workspaces.
            None if matches!(scope, tilo_actions::remote::WatchScope::Records) => {
                self.account.changed()
            }
            None => None,
        }
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match &self.actions {
            Some(actions) if !self.account.knows(caller, id) => actions.cancel(caller, id),
            _ => self.account.cancel(caller, id),
        }
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.actions.as_ref()?.content_puts()
    }

    /// What a put is for decides where it goes, as the actions this joins decide it
    /// ([`tilo_actions::remote::ActionProvider::content_puts_for`]).
    fn content_puts_for(
        &self,
        target: Option<&tilo_actions::remote::RemoteActionTarget>,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.actions.as_ref()?.content_puts_for(target)
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.actions.as_ref()?.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.actions.as_ref()?.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.actions.as_ref()?.resource_reads()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.actions.as_ref()?.resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.actions.as_ref()?.change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        match &self.actions {
            Some(actions) => actions.installed_surface(app_id),
            None => Pending::ready(None),
        }
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        match &self.actions {
            Some(actions) => actions.invocation(address, argument),
            None => self.account.invocation(address, argument),
        }
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match &self.actions {
            Some(actions) if !Self::is_account(&request.target) => {
                actions.run(caller, request, admitted)
            }
            _ => ActionProvider::run(&self.account, caller, request, admitted),
        }
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        match &self.actions {
            Some(actions) if !Self::is_account(&request.target) => {
                actions.asks_first(caller, request, admitted)
            }
            _ => self.account.asks_first(caller, request, admitted),
        }
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        match &self.actions {
            Some(actions) if !Self::is_account(&request.target) => actions.watch(caller, request),
            _ => self.account.watch(caller, request),
        }
    }
}

impl Server {
    /// Delete the workspace `args` names, and every key that reached only it.
    async fn delete_workspace(&self, grant: &Grant, key: &Connection, args: &Value) -> Outcome {
        let Some(workspace) = field(args, "workspace") else {
            return failed(
                "invalid_arguments",
                "`workspace` is required",
                "pass {workspace: \"<id>\"}; account/workspaces#list names them",
            );
        };
        if let Err(error) = key.server().delete(key, workspace).await {
            return refused(&error);
        }
        // Every key that reached only that workspace goes with it.
        let revoked = self.revoke_keys_for(&grant.person, workspace).await;
        Outcome::Ok(json!({"deleted": workspace, "revoked_keys": revoked}))
    }

    /// Ask for the key `args` names (`name`, and optionally `workspace`,
    /// `expires_in` and `browser_pages`), for the person to approve.
    fn create_key(&self, args: &Value) -> Outcome {
        let asked = crate::keys::expires_in_of(args.get("expires_in"))
            .and_then(|expires_in| {
                crate::keys::browser_pages_of(args.get("browser_pages"))
                    .map(|browser_pages| (expires_in, browser_pages))
            })
            .and_then(|(expires_in, browser_pages)| {
                self.request_key(
                    field(args, "name"),
                    field(args, "workspace"),
                    expires_in,
                    browser_pages,
                )
            });
        match asked {
            Ok((poll, _)) => Outcome::Key(poll),
            Err(why) => failed(
                "invalid_arguments",
                why,
                "pass {name, workspace?, expires_in?, browser_pages?}",
            ),
        }
    }

    /// Revoke the key `args` names, and have the workspace it reached keep the
    /// revocation too.
    async fn revoke_key(&self, grant: &Grant, key: &Connection, args: &Value) -> Outcome {
        let Some(id) = field(args, "grant") else {
            return failed(
                "invalid_arguments",
                "`grant` is required",
                "tilo_ls record:tilo.grants lists them in each workspace",
            );
        };
        let held_here = self
            .store
            .grant(id)
            .filter(|found| found.person == grant.person);
        let minted = self
            .store
            .grant(id)
            .is_some_and(|found| found.parent() == Some(grant.id.as_str()));
        // A key of the person's own reaching all their workspaces revokes any key of theirs,
        // wherever it was made: each workspace that lists it keeps the revocation.
        if grant.scope == Scope::Account && persons_own(grant) && id != grant.id && !minted {
            let workspaces = match key.server().labelled(key, &WorkspaceLabels::new()).await {
                Ok(workspaces) => workspaces,
                Err(error) => return refused(&error),
            };
            // Each workspace that admitted the key keeps its grant, and records its revocation;
            // one that never admitted it has nothing to record.
            let mut recorded = false;
            for workspace in workspaces {
                match revoke_in(key, &workspace.id, id).await {
                    Ok(changed) => recorded |= changed,
                    Err(outcome) => {
                        tracing::warn!(
                            workspace = workspace.id,
                            grant = id,
                            "a workspace did not keep a key's revocation"
                        );
                        return outcome;
                    }
                }
            }
            if held_here.is_none() && !recorded {
                return failed(
                    "not_found",
                    "no such key of yours",
                    "tilo_ls record:tilo.grants lists them in each workspace",
                );
            }
            if held_here.is_some()
                && let Err(error) = self.revoke_grant(&grant.person, id).await
            {
                tracing::warn!(%error, grant = id, "a key's revocation was not kept");
                return failed(
                    "unavailable",
                    "revocation is unavailable",
                    "try again shortly",
                );
            }
            return Outcome::Ok(json!({"revoked": id}));
        }
        let own = id == grant.id || minted;
        if !own {
            return failed(
                "not_found",
                "no such key of this key",
                "tilo_ls record:tilo.grants lists them in each workspace",
            );
        }
        // Where the key is recorded: the workspace it reaches, or the
        // workspace the calling key itself reaches.
        let scope = self
            .store
            .grant(id)
            .map_or_else(|| grant.scope.clone(), |found| found.scope);
        let workspace = match scope {
            Scope::Workspace(workspace) => Some(workspace),
            Scope::Account => None,
        };
        match self.revoke_grant(&grant.person, id).await {
            Ok(_) => {
                // The workspace keeps the revocation, so every holder and API
                // server refuses the key, not only this one.
                if let Some(workspace) = workspace
                    && let Err(outcome) = revoke_in(key, &workspace, id).await
                {
                    tracing::warn!(
                        workspace,
                        grant = id,
                        "the workspace did not record a key's revocation"
                    );
                    return outcome;
                }
                Outcome::Ok(json!({"revoked": id}))
            }
            Err(_) => failed(
                "unavailable",
                "revocation is unavailable",
                "try again shortly",
            ),
        }
    }

    /// Revoke every live key of `person` that reaches only `workspace`,
    /// deleted: their sessions end now. Answers the grants revoked.
    async fn revoke_keys_for(&self, person: &str, workspace: &str) -> Vec<String> {
        let mut revoked = Vec::new();
        for grant in self.store.grants_of(person) {
            if grant.scope != Scope::Workspace(workspace.to_owned()) {
                continue;
            }
            match self.revoke_grant(person, &grant.id).await {
                Ok(true) => revoked.push(grant.id),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, grant = %grant.id, "a key of a deleted workspace stays");
                }
            }
        }
        revoked
    }

    /// Mint a key reaching one workspace, for an agent to act there.
    async fn mint(
        self: &Arc<Self>,
        grant: &Grant,
        connection: &Connection,
        args: &Value,
    ) -> Outcome {
        let Some(workspace) = args.get("workspace").and_then(Value::as_str) else {
            return failed(
                "invalid_arguments",
                "`workspace` is required",
                "account/workspaces#list names them",
            );
        };
        let Some(AgentScope::Workspace(id)) = AgentScope::parse(workspace) else {
            return failed(
                "invalid_arguments",
                "`workspace` is a workspace's id",
                "account/workspaces#list names them",
            );
        };
        let found = match connection.server().workspace(connection, workspace).await {
            Ok(found) => found,
            Err(error) => return refused(&error),
        };
        let Some(found) = found else {
            return refused(&ServerError::NotFound);
        };
        let key = &connection.key;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(
                || format!("{}: {}", grant.client_name, found.name),
                str::to_owned,
            );
        let name: String = name
            .chars()
            .filter(|character| !character.is_control())
            .take(crate::keys::MAX_NAME_CHARS)
            .collect();
        let now = now_ms();
        let minted = match mint_key(
            key,
            id,
            &name,
            args.get("expires_in").and_then(Value::as_u64),
            now,
        ) {
            Ok(minted) => minted,
            Err(outcome) => return outcome,
        };
        let kind = GrantKind::Minted {
            parent: grant.id.clone(),
        };
        let Ok((minted, token)) = self.store.resource_key(&minted, &name, kind, None, now) else {
            return failed(
                "unavailable",
                "the key could not be kept",
                "try again shortly",
            );
        };
        // The workspace keeps the new key's certificate before anyone holds it, so its first
        // read is answered as that key's own.
        self.admit(&minted).await;
        Outcome::Ok(json!({
            "grant": minted.id,
            "name": minted.client_name,
            "token": token,
            "workspace": workspace,
            "mcp_url": format!("{}/w/{workspace}/mcp", self.config.issuer),
            "expires_at_ms": minted.expires_ms,
        }))
    }
}

/// Whether `grant` is one of the person's own keys rather than an agent's grant: an API key they
/// approved, or the key a computer of theirs logged in with. An agent never sees or takes back the
/// person's other keys.
fn persons_own(grant: &Grant) -> bool {
    matches!(grant.kind, GrantKind::Key)
}

/// Record `grant` revoked in `workspace`: whether it changed the workspace.
async fn revoke_in(connection: &Connection, workspace: &str, grant: &str) -> Result<bool, Outcome> {
    let mut arguments = Map::new();
    arguments.insert("grant".to_owned(), json!(grant));
    match governance(connection, workspace, REVOKE_KEY, arguments).await {
        Ok(_) => Ok(true),
        // A workspace that holds no such grant, or already refuses the key as
        // revoked, has nothing left to record.
        Err(Outcome::Failed { code, .. }) if code == "not_found" || code == "key_revoked" => {
            Ok(false)
        }
        Err(outcome) => Err(outcome),
    }
}

/// The key's webhooks: a record of the workspace that keeps it, registered and removed through
/// the workspace's own verbs, and posted by the device that keeps it.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per webhook action, each a few lines of arguments and an answer"
)]
async fn forwarded_webhooks(connection: &Connection, op: Op, args: &Value) -> Outcome {
    let cert = connection.key.cert();
    let caller = tilo_peer_state::key_caller::caller_of_certificate(cert);
    let named = field(args, "workspace").map(str::to_owned);
    let workspaces = match named.clone() {
        Some(workspace) => vec![workspace],
        None => match connection
            .server()
            .labelled(connection, &WorkspaceLabels::new())
            .await
        {
            Ok(listed) => listed.into_iter().map(|workspace| workspace.id).collect(),
            Err(error) => return refused(&error),
        },
    };
    match op {
        Op::ListWebhooks => {
            let mut rows = Vec::new();
            for workspace in &workspaces {
                let Ok(answer) =
                    workspace_verb_as(connection, workspace, LIST_WEBHOOKS, Map::new(), &caller)
                        .await
                else {
                    continue;
                };
                rows.extend(
                    answer
                        .get("webhooks")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            Outcome::Ok(json!({"webhooks": rows}))
        }
        Op::CreateWebhook => {
            let Some(workspace) = named else {
                return failed(
                    "invalid_arguments",
                    "`workspace` is required",
                    "pass {workspace, url, events?, path?}; account/workspaces#list names them",
                );
            };
            let Some(url) = field(args, "url") else {
                return failed(
                    "invalid_arguments",
                    "`url` is required",
                    "pass {workspace, url, events?, path?}",
                );
            };
            if let Some(why) = tilo_net::address::webhook_refusal(url) {
                return failed(
                    "invalid_arguments",
                    why,
                    "name an https URL on the public internet, or an address of this computer",
                );
            }
            let mut arguments = Map::new();
            arguments.insert("url".into(), json!(url));
            if let Some(events) = args.get("events").filter(|events| events.is_array()) {
                arguments.insert("events".into(), events.clone());
            }
            if let Some(path) = field(args, "path") {
                arguments.insert("path".into(), json!(path));
            }
            match workspace_verb_as(connection, &workspace, CREATE_WEBHOOK, arguments, &caller)
                .await
            {
                Ok(made) => Outcome::Ok(made),
                Err(outcome) => outcome,
            }
        }
        Op::DeleteWebhook => {
            let Some(webhook) = field(args, "webhook") else {
                return failed(
                    "invalid_arguments",
                    "`webhook` is required",
                    "account/webhooks#list names them",
                );
            };
            for workspace in &workspaces {
                let mut arguments = Map::new();
                arguments.insert("webhook".into(), json!(webhook));
                match workspace_verb_as(connection, workspace, DELETE_WEBHOOK, arguments, &caller)
                    .await
                {
                    Ok(deleted) => return Outcome::Ok(deleted),
                    Err(Outcome::Failed {
                        code: "not_found" | "refused",
                        ..
                    }) => {}
                    Err(outcome) => return outcome,
                }
            }
            failed(
                "not_found",
                "no such webhook of this key",
                "account/webhooks#list names them",
            )
        }
        _ => Outcome::Failed {
            code: "unavailable",
            message: "that is no webhook action".into(),
            next: String::new(),
        },
    }
}

/// Run the workspace's own verb `action` in `workspace` as `caller`, and answer its output.
async fn workspace_verb_as(
    connection: &Connection,
    workspace: &str,
    action: &str,
    arguments: Map<String, Value>,
    caller: &Caller,
) -> Result<Value, Outcome> {
    let id = connection
        .server()
        .enter(connection, workspace)
        .await
        .map_err(|error| refused(&error))?;
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let unanswered = || {
        failed(
            "unavailable",
            "the workspace did not answer",
            "try again shortly",
        )
    };
    let receipt = connection
        .device
        .actions(id)
        .invoke(caller, request)
        .await
        .ok_or_else(unanswered)?
        .map_err(|error| refused_key(&error))?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(refused_key(&error)),
        ReceiptStatus::Failed { error } => {
            Err(failed("unavailable", error.message, "try again shortly"))
        }
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => Err(unanswered()),
    }
}

/// [`workspace_verb_as`] as the API server itself asks a workspace, for the calls it makes with no
/// key behind them (a key's own record of its grants).
async fn governance(
    connection: &Connection,
    workspace: &str,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, Outcome> {
    workspace_verb_as(
        connection,
        workspace,
        action,
        arguments,
        &Caller::local_person("tilo api"),
    )
    .await
}

/// What a workspace's refusal of a key's listing or revocation, `error`, tells the caller.
fn refused_key(error: &ActionCallError) -> Outcome {
    match error {
        // The workspace records its keys' revocations: one it refuses as
        // revoked asks its person for another.
        ActionCallError::Refused { reason } if reason.contains("key_revoked") => {
            failed("key_revoked", "this key was revoked", crate::APPROVE_AGAIN)
        }
        ActionCallError::UnknownAction => failed(
            "not_found",
            "the workspace holds no such key, or it is not this key's to revoke",
            "tilo_ls record:tilo.grants lists the keys this one holds and the keys it minted",
        ),
        ActionCallError::NotPermitted => failed(
            "not_permitted",
            "the workspace does not let this key revoke that one",
            "only the key itself and the key that minted it revoke it",
        ),
        other => failed("unavailable", other.to_string(), "try again shortly"),
    }
}

/// The hosts calls may be forwarded to, as an account lists them: the ones
/// the API server knows, its first the default.
fn host_rows(server: &Server) -> Value {
    Value::Array(
        server
            .hosts
            .names()
            .into_iter()
            .enumerate()
            .map(|(index, name)| json!({"name": name, "default": index == 0}))
            .collect(),
    )
}

/// Whether `workspace` is labelled with every one of `labels`.
pub(crate) fn labelled(workspace: &crate::server::Workspace, labels: &WorkspaceLabels) -> bool {
    labels
        .iter()
        .all(|(name, value)| workspace.labels.get(name) == Some(value))
}

/// The argument `args` names, trimmed, when it holds text.
fn field<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The workspaces `key` reaches that are labelled with every one of the `labels` `args` names,
/// with the hosts it may create on.
async fn workspaces(key: &Connection, server: &Server, args: &Value) -> Outcome {
    let labels = match labels_of(args) {
        Ok(labels) => labels,
        Err(refused) => return refused,
    };
    match key.server().labelled(key, &labels).await {
        Ok(mut workspaces) => {
            workspaces.retain(|workspace| labelled(workspace, &labels));
            Outcome::Ok(json!({"workspaces": workspaces, "hosts": host_rows(server)}))
        }
        Err(error) => refused(&error),
    }
}

/// What a device that founds no workspace itself answers a create with when none of its person's
/// devices founds their workspaces: the code of its refusal.
pub const NO_FOUNDING_DEVICE: &str = "no_founding_device";
/// What it answers a create with when the person's device that founds their workspaces did not
/// answer in time: the code of its refusal.
pub const FOUNDING_DEVICE_UNREACHED: &str = "founding_device_unreached";

/// Make the workspace `args` asks for on the person's device that founds their workspaces, as
/// [`crate::session::Here::create_where_founded`] asks it: its answer as that device gave it.
async fn create_where_founded(here: &dyn Here, args: &Value) -> Outcome {
    let Some(name) = field(args, "name") else {
        return failed(
            "invalid_arguments",
            "`name` is required",
            "pass {name: \"Applicant: Jane Doe\"}",
        );
    };
    let (id, labels) = match (chosen_id(args), labels_of(args)) {
        (Ok(id), Ok(labels)) => (id, labels),
        (Err(refused), _) | (_, Err(refused)) => return refused,
    };
    let creating = crate::session::Creating {
        name: name.to_owned(),
        labels,
        id,
    };
    match here.create_where_founded(creating).await {
        Ok(answer) => Outcome::Ok(answer),
        Err(crate::session::NotFounded::NoFoundingDevice) => failed(
            NO_FOUNDING_DEVICE,
            "none of your devices makes your workspaces",
            "make the workspace on the computer you first set Tilo up on",
        ),
        Err(crate::session::NotFounded::Unreached { waited }) => failed(
            FOUNDING_DEVICE_UNREACHED,
            format!(
                "the device that makes your workspaces didn't answer within {} seconds",
                waited.as_secs()
            ),
            "turn that device on and connect it to a network, then try again",
        ),
        Err(crate::session::NotFounded::Refused(why)) => failed(
            "refused",
            format!("the device that makes your workspaces didn't make this one: {why}"),
            "fix what it names and try again",
        ),
    }
}

/// What a desktop answers `op` with `args`: the workspaces it keeps, kept or left at the
/// person's word, and everything else but making one as the person's account at their server
/// answers it. A workspace branched there is kept here too, and one deleted there is kept here
/// no more.
/// Join `workspace` through its open entry at `host`, as its directory publishes the host: the
/// device signs its own admission, and the entry's refusal is said in its own words.
async fn join_entry(here: &dyn Here, workspace: &str, host: Option<&Value>) -> Outcome {
    let Some(host) = host else {
        return failed(
            "invalid_arguments",
            "`host` is required with `workspace`",
            "pass the host the workspace's directory publishes: {hostCert, address}",
        );
    };
    let host = match serde_json::from_value::<tilo_server_protocol::web::PageHost>(host.clone()) {
        Ok(host) => host,
        Err(error) => {
            return failed(
                "invalid_arguments",
                format!("`host` is not a host as a directory publishes one: {error}"),
                "pass the host the workspace's directory publishes: {hostCert, address}",
            );
        }
    };
    match here.join_entry(workspace, host).await {
        Ok(workspace) => Outcome::Ok(json!({"workspace": workspace, "kept_here": true})),
        Err(why) => failed(
            "refused",
            why,
            "ask someone in the workspace for an invitation",
        ),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per action a desktop answers for its person, each a few lines"
)]
async fn here_answers(
    here: &dyn Here,
    key: &tilo_user_identity::DeviceCert,
    server: &Server,
    op: Op,
    args: &Value,
) -> Outcome {
    let needs_workspace = || {
        failed(
            "invalid_arguments",
            "`workspace` is required",
            "pass {workspace: \"<id>\"}; account/workspaces#list names them",
        )
    };
    match op {
        Op::ListWorkspaces => return listed_here(here, key, server, args).await,
        Op::EnterWorkspace | Op::LeaveWorkspace => {
            let Some(workspace) = field(args, "workspace") else {
                return needs_workspace();
            };
            let entering = op == Op::EnterWorkspace;
            let done = if entering {
                here.keep(workspace).await
            } else {
                here.leave(workspace).await
            };
            return match done {
                Ok(()) => Outcome::Ok(json!({"workspace": workspace, "kept_here": entering})),
                Err(why) => failed("unavailable", why, "try again shortly"),
            };
        }
        Op::JoinWorkspace => {
            if let Some(workspace) = field(args, "workspace") {
                return join_entry(here, workspace, args.get("host")).await;
            }
            let Some(invite) = field(args, "invite") else {
                return failed(
                    "invalid_arguments",
                    "`invite` or `workspace` is required",
                    "pass {invite: \"<the invitation>\"}, or {workspace, host} for a workspace \
                     whose entry is open",
                );
            };
            return match here.join(invite).await {
                Ok(workspace) => Outcome::Ok(json!({"workspace": workspace, "kept_here": true})),
                Err(why) => failed(
                    "refused",
                    why,
                    "ask whoever invited you for a new invitation",
                ),
            };
        }
        // A webhook is a record of the workspace that keeps it: only the device keeping it
        // registers one, and this device answers its person's account some other way.
        Op::ListWebhooks | Op::CreateWebhook | Op::DeleteWebhook => {
            return failed(
                "unavailable",
                "a webhook is kept by the device that keeps its workspace",
                "ask it on the device that keeps the workspace",
            );
        }
        _ => {}
    }
    // A workspace is founded on the person's device that founds their own: this device founds
    // none, so it asks that one, and never their server, which founds none for it.
    if op == Op::CreateWorkspace {
        return create_where_founded(here, args).await;
    }
    let Some(account) = here.account().await else {
        return refused(&ServerError::NoAccountHere);
    };
    let asked = match op {
        Op::RenameWorkspace | Op::Unattended | Op::Usage | Op::AskModel => account_target(op, args),
        _ => server_operation(op, args).map(|(action, arguments)| (host(action), arguments)),
    };
    let (target, arguments) = match asked {
        Ok(asked) => asked,
        Err(outcome) => return outcome,
    };
    let answer = match account.call(target, arguments.clone()).await {
        Ok(answer) => answer,
        Err(why) => return failed("unavailable", why, "try again shortly"),
    };
    let Some(workspace) = arguments
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|_| matches!(op, Op::BranchWorkspace | Op::DeleteWorkspace))
    else {
        return Outcome::Ok(answer);
    };
    if op == Op::DeleteWorkspace {
        if here.workspaces(key).iter().any(|kept| kept.id == workspace)
            && let Err(why) = here.leave(workspace).await
        {
            tracing::warn!(%why, %workspace, "a deleted workspace's copy is still kept here");
        }
        return Outcome::Ok(json!({"deleted": workspace}));
    }
    let made = match serde_json::from_value::<CreatedWorkspace>(answer) {
        Ok(made) => made.workspace,
        Err(error) => {
            return failed(
                "unavailable",
                format!("the server answered no workspace: {error}"),
                "try again shortly",
            );
        }
    };
    let kept = here.keep(&made.workspace).await;
    if let Err(why) = &kept {
        tracing::warn!(%why, workspace = %made.workspace, "a workspace made from this device is not kept here");
    }
    let workspace = crate::server::Workspace {
        kept_here: kept.is_ok(),
        ..crate::server::Workspace::listed(&made, None)
    };
    Outcome::Ok(json!({ "workspace": workspace }))
}

/// The server's own operation `action`.
fn host(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, action),
    }
}

/// What a computer asks its server for `op` with `args` beyond the person's workspaces
/// themselves, as a key asks it over its connection: renaming a workspace and turning its
/// unattended runs on or off as its governance decides, the account's usage, and a turn of the
/// person's model.
fn account_target(
    op: Op,
    args: &Value,
) -> Result<(RemoteActionTarget, Map<String, Value>), Outcome> {
    let needs =
        |what: &str, next: &str| failed("invalid_arguments", format!("{what} required"), next);
    let mut arguments = Map::new();
    let target = match op {
        Op::RenameWorkspace => {
            let (Some(workspace), Some(name)) = (field(args, "workspace"), field(args, "name"))
            else {
                return Err(needs(
                    "`workspace` and `name` are",
                    "pass {workspace: \"<id>\", name: \"Acme\"}",
                ));
            };
            arguments.insert("name".into(), json!(name.trim()));
            RemoteActionTarget::workspace_governance(workspace, RENAME)
        }
        Op::Unattended => {
            let (Some(workspace), Some(on)) = (
                field(args, "workspace"),
                args.get("on").and_then(Value::as_bool),
            ) else {
                return Err(needs(
                    "`workspace` and `on` are",
                    "pass {workspace, on: true}",
                ));
            };
            arguments.insert("on".into(), json!(on));
            RemoteActionTarget::workspace_governance(workspace, RUN_UNATTENDED)
        }
        Op::AskModel => {
            let (Some(_), Some(Value::Object(request))) =
                (field(args, "workspace"), args.get("request"))
            else {
                return Err(needs(
                    "`workspace` and `request` are",
                    "pass {workspace, request: {messages, …}}",
                ));
            };
            arguments.clone_from(request);
            model_providers::models::ask()
        }
        _ => tilo_action_services::server_account::target(
            tilo_action_services::server_account::ACCOUNT,
        ),
    };
    Ok((target, arguments))
}

/// The server's own operation that answers `op`, asked with `args`, and its arguments: making,
/// branching and deleting the person's workspaces. A computer asks its server nothing else
/// itself; a key asks the rest over its connection.
fn server_operation(op: Op, args: &Value) -> Result<(&'static str, Map<String, Value>), Outcome> {
    let (id, labels) = (chosen_id(args)?, labels_of(args)?);
    let mut arguments = Map::new();
    if !labels.is_empty() {
        arguments.insert("labels".into(), json!(labels));
    }
    let made = id.unwrap_or_else(uuid::Uuid::new_v4).to_string();
    let named = |what: &str| {
        field(args, what).map(|text| json!(text)).ok_or_else(|| {
            failed(
                "invalid_arguments",
                format!("`{what}` is required"),
                "account/workspaces#list names the workspaces and what each is called",
            )
        })
    };
    let action = match op {
        Op::CreateWorkspace => {
            arguments.insert("name".into(), named("name")?);
            arguments.insert("workspace".into(), json!(made));
            CREATE_OWN_WORKSPACE
        }
        Op::BranchWorkspace => {
            arguments.insert("workspace".into(), named("workspace")?);
            arguments.insert("name".into(), named("name")?);
            arguments.insert("branch".into(), json!(made));
            BRANCH_OWN_WORKSPACE
        }
        Op::DeleteWorkspace => {
            arguments.insert("workspace".into(), named("workspace")?);
            DELETE_WORKSPACE
        }
        _ => {
            return Err(failed(
                "not_permitted",
                "a computer's own keys are made on it with `tilo keys`, not asked of its server",
                "run `tilo keys` on this computer",
            ));
        }
    };
    Ok((action, arguments))
}

/// The workspaces a desktop keeps, and those of the person's its server lists that it does
/// not keep, labelled with every one of the `labels` `args` names, with the hosts it may create on.
async fn listed_here(
    here: &dyn Here,
    key: &tilo_user_identity::DeviceCert,
    server: &Server,
    args: &Value,
) -> Outcome {
    let labels = match labels_of(args) {
        Ok(labels) => labels,
        Err(refused) => return refused,
    };
    let mut workspaces = here.workspaces(key);
    if let Some(account) = here.account().await {
        match account.listed(&key.user_public_key, &labels, None).await {
            Ok(listed) => {
                for row in listed
                    .workspaces
                    .into_iter()
                    .map(crate::server::Workspace::record)
                {
                    match workspaces.iter_mut().find(|kept| kept.id == row.id) {
                        Some(kept) => {
                            *kept = crate::server::Workspace {
                                kept_here: true,
                                ..row
                            };
                        }
                        None => workspaces.push(row),
                    }
                }
            }
            Err(why) => {
                tracing::debug!(%why, "the server's list is not in this device's");
            }
        }
    }
    workspaces.retain(|workspace| labelled(workspace, &labels));
    Outcome::Ok(json!({"workspaces": workspaces, "hosts": host_rows(server)}))
}

/// Name the workspace `args` names (`workspace`) `name`, as `key`: the
/// server decides by the workspace's governance, as for the person's own page.
async fn rename_workspace(key: &Connection, args: &Value) -> Outcome {
    let (Some(workspace), Some(name)) = (field(args, "workspace"), field(args, "name")) else {
        return failed(
            "invalid_arguments",
            "`workspace` and `name` are required",
            "pass {workspace: \"<id>\", name: \"Acme\"}; account/workspaces#list names them",
        );
    };
    match key.server().rename(key, workspace, name).await {
        Ok(workspace) => Outcome::Ok(json!({"workspace": workspace})),
        Err(crate::server::Administration::Refused(why)) => failed(
            "not_permitted",
            format!("the workspace refused: {why}"),
            "its owners and moderators name it",
        ),
        Err(crate::server::Administration::Unanswered(why)) => {
            failed("unavailable", why, "try again shortly")
        }
    }
}

/// The labels `args` names (`labels`, names to text values): none when it
/// names none.
fn labels_of(args: &Value) -> Result<WorkspaceLabels, Outcome> {
    let labels = match args.get("labels") {
        None | Some(Value::Null) => return Ok(WorkspaceLabels::new()),
        Some(labels) => {
            serde_json::from_value::<WorkspaceLabels>(labels.clone()).map_err(|_| {
                failed(
                    "invalid_arguments",
                    "`labels` names text values: {\"customer_id\": \"cus_8121\"}",
                    "pass labels as an object of names to text",
                )
            })?
        }
    };
    tilo_server_protocol::workspaces::check_labels(&labels)
        .map_err(|why| failed("invalid_arguments", why, "fix the label and ask again"))?;
    Ok(labels)
}

/// The id a caller chose for a workspace it makes (`id`), so that a retry
/// asks for the same one: none when it chose none.
fn chosen_id(args: &Value) -> Result<Option<uuid::Uuid>, Outcome> {
    args.get("id")
        .and_then(Value::as_str)
        .map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| {
            failed(
                "invalid_arguments",
                "`id` is a workspace id (a UUID)",
                "leave `id` out to make a new workspace",
            )
        })
}

/// A key for the workspace `id` alone, named `name`, certified by `key`: a
/// grant for one workspace that ends after `expires_in` seconds, with `key`,
/// or after the most a grant may last, whichever is soonest. A server key
/// that never expires still mints keys that do.
fn mint_key(
    key: &GrantKey,
    id: [u8; 16],
    name: &str,
    expires_in: Option<u64>,
    now: u64,
) -> Result<GrantKey, Outcome> {
    let now_ns = now.saturating_mul(1_000_000);
    let most = now_ns.saturating_add(tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS);
    let limit = key
        .cert()
        .expires_at_ns
        .map_or(most, |parent| parent.min(most));
    let expires_ns = expires_in.map_or(limit, |seconds| {
        now_ns
            .saturating_add(seconds.saturating_mul(1_000_000_000))
            .min(limit)
    });
    if expires_ns <= now_ns {
        return Err(failed(
            "expired",
            "this key has expired",
            crate::APPROVE_AGAIN,
        ));
    }
    let minted_key = GrantKey::generate().map_err(|_| {
        failed(
            "unavailable",
            "no randomness is available",
            "try again shortly",
        )
    })?;
    let mut cert = tilo_user_identity::prepare_minted_grant(
        key.cert(),
        minted_key.verifying_key().to_bytes(),
        id,
        name,
        now_ns,
        expires_ns,
    );
    tilo_user_identity::sign_device_cert(&mut cert, key.key()).map_err(|error| {
        failed(
            "not_permitted",
            format!("this key cannot mint: {error}"),
            crate::APPROVE_AGAIN,
        )
    })?;
    // A key the person made mints keys of theirs, as its certificate says.
    let (server_key, browser_pages) = (
        cert.capabilities.server_key,
        cert.capabilities.browser_pages,
    );
    GrantKey::new(
        minted_key,
        cert,
        &AgentScope::Workspace(id),
        server_key,
        browser_pages,
        now_ns,
    )
    .map_err(|error| failed("not_permitted", error.0, crate::APPROVE_AGAIN))
}
