//! The account: a person's workspaces, their keys, what their workspaces
//! keep, and their model, published as one provider ([`Account`], app
//! [`PROVIDER`], reached by the path `account`) in every tree a key walks.
//! An agent, a program and a route of the API reach it as they reach any
//! action: by path, through the one walk of the tree.
//!
//! - `account/workspaces#list {labels?}`, `#create {name, labels?, id?}`,
//!   `#branch {workspace, name, labels?, id?}`, `#rename {workspace, name}`,
//!   `#delete {workspace}`, `#unattended {workspace, on}`;
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
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, pending,
};
use tilo_actions::{ActionAddress, ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
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

/// The account's actions, declared once, and the surface they publish.
mod actions;

use actions::{Asked, KeyAction, ModelAction, UsageAction, WebhookAction, WorkspaceAction};

/// The app the account is published as: `account` names it.
pub const PROVIDER: &str = "tilo.account";
/// The person's workspaces.
pub const WORKSPACES: &str = "account/workspaces";
/// The keys to them.
pub const KEYS: &str = "account/keys";
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
    /// A workspace's host keeps it for its person's yes, as this state says, and the account
    /// follows it there until they answer.
    Awaiting(InvocationState),
    /// The person is asked for a key on the device that answers for their account.
    Certifying(Box<Certifying>),
    /// It ended so.
    Ended(InvocationState),
}

/// What an operation the person is asked about answers once they decide.
type Decided = std::pin::Pin<Box<dyn std::future::Future<Output = Outcome> + Send>>;

/// What an action answered.
pub(crate) enum Outcome {
    Ok(Value),
    /// Asked of the person: the poll secret of the key they are asked for.
    Key(String),
    /// Asked of the person where their account is answered, for a key it certifies.
    Certifying(Box<Certifying>),
    /// Asked of a workspace's host, which runs it on.
    Asked(SessionActions),
    /// Asked of a workspace's host, which holds it for the person to allow: the caller is
    /// answered that it waits for them, and the same invocation answers how they decided.
    Held(SessionActions),
    /// Run at a provider, recorded so its caller follows it there by its receipt.
    Ran(Box<InvocationReceipt>),
    /// Asked of the server, which keeps it for the person's yes in the record named: the caller
    /// follows it here, and it ends as the server answers once they decide.
    Awaiting(tilo_actions::remote::asked::RecordRef, Decided),
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
        ServerError::AwaitingApproval(..) => failed(
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

/// How long one follow of an operation the person is asked about waits for their answer before
/// the account looks whether it still keeps the invocation.
const DECIDING: std::time::Duration = std::time::Duration::from_secs(60);

/// The state invocation `id` ends in once it answered `outcome`.
fn ended(id: InvocationId, outcome: Outcome) -> InvocationState {
    let update = match outcome {
        Outcome::Ok(value) => InvocationUpdate::Succeeded {
            invocation: id,
            output: ActionOutput {
                value: Some(value),
                ..ActionOutput::default()
            },
        },
        Outcome::Failed {
            code,
            message,
            next,
        } => InvocationUpdate::Failed {
            invocation: id,
            error: failure(code, message, &next),
        },
        _ => InvocationUpdate::Failed {
            invocation: id,
            error: failure(
                "unavailable",
                "the server answered nothing the operation ends with".to_owned(),
                "try again",
            ),
        },
    };
    InvocationState::new(id, update)
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

    /// The page `query` asks of the account, in the tree of `workspace`: the key it names when
    /// it asks for the exact action on one ([`actions::key_row`]), the declared actions otherwise.
    fn ask(query: &CatalogueQuery, workspace: &str) -> CataloguePage {
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
        let rows = match query
            .target
            .as_ref()
            .and_then(|target| key_of(&target.resource))
        {
            Some(grant) => actions::key_row(grant).into_iter().collect(),
            None => actions::declarations()
                .map(|(resource, label, declaration)| actions::row(resource, label, &declaration))
                .collect(),
        };
        let mut page = CataloguePage::from_rows(REVISION.into(), rows, query);
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
            InvokeArgument::Json(arguments) => arguments.clone(),
            _ => Map::new(),
        };
        let address = &request.target.address;
        let outcome = match Asked::decode(&address.resource, &address.action, &args) {
            Some(Ok(asked)) => self.answer(&asked, id).await,
            Some(Err(error)) => failed(
                "invalid_arguments",
                error.to_string(),
                "pass the arguments the action declares, and nothing else",
            ),
            None => {
                return InvocationReceipt {
                    invocation: id,
                    status: ReceiptStatus::Rejected {
                        error: ActionCallError::UnknownAction,
                    },
                };
            }
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
            Outcome::Certifying(certifying) => {
                self.keep(id, Run::Certifying(certifying));
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
            Outcome::Awaiting(ask, decided) => {
                self.keep(id, Run::Awaiting(InvocationState::new(id, ask.waiting(id))));
                let account = self.clone();
                tokio::spawn(async move {
                    let outcome = decided.await;
                    account.keep(id, Run::Ended(ended(id, outcome)));
                });
                ReceiptStatus::Accepted
            }
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

    /// Admit `asked` for this key, and run it.
    #[expect(
        clippy::too_many_lines,
        reason = "one match over the account's actions, each a few lines of an answer"
    )]
    async fn answer(&self, asked: &Asked, id: InvocationId) -> Outcome {
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
                answer: answer_for(&grant, asked),
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
            Reach::Here(here, key) => {
                // A device that serves the person's account itself answers their calls here, as
                // the key they were made with.
                if let Some(outcome) = self.answer_here(here.as_ref(), &grant, asked, id).await {
                    return outcome;
                }
                return here_answers(here.as_ref(), key.cert(), &server, asked).await;
            }
        };
        match asked {
            Asked::Revoke(revoked) => {
                revoke(&server, &grant, revoked, Recorder::Server(&key), true).await
            }
            Asked::Workspaces(WorkspaceAction::List { labels }) => {
                workspaces(&key, &server, labels.as_ref()).await
            }
            Asked::Workspaces(
                WorkspaceAction::Enter { .. }
                | WorkspaceAction::Leave { .. }
                | WorkspaceAction::Join { .. },
            ) => failed(
                "not_permitted",
                "only one of the person's devices keeps workspaces",
                "ask it on the device that is to keep the workspace",
            ),
            Asked::Workspaces(WorkspaceAction::Create { .. } | WorkspaceAction::Branch { .. }) => {
                self.make(key, asked, id).await
            }
            Asked::Workspaces(WorkspaceAction::Rename { workspace, name }) => {
                rename_workspace(&key, workspace, name).await
            }
            Asked::Workspaces(WorkspaceAction::Delete { workspace }) => {
                server
                    .delete_workspace(self, (&grant, &key), (id, workspace.trim()))
                    .await
            }
            Asked::Workspaces(WorkspaceAction::Unattended { workspace, .. }) => {
                self.unattended(&key, id, workspace, asked).await
            }
            Asked::Keys(KeyAction::Create {
                name,
                workspace,
                expires_in,
                browser_pages,
            }) => server.create_key(
                name,
                workspace.as_deref(),
                *expires_in,
                browser_pages.unwrap_or(true),
            ),
            Asked::Keys(KeyAction::Mint {
                workspace,
                name,
                expires_in,
            }) => {
                server
                    .mint(&grant, &key, workspace, name.as_deref(), *expires_in)
                    .await
            }
            Asked::Usage(UsageAction::Read) => match key.server().account(&key).await {
                Ok(account) => Outcome::Ok(account),
                Err(error) => refused(&error),
            },
            Asked::Models(ModelAction::Ask { workspace, .. }) => {
                match self.ask_host(&key, workspace, id, asked, None).await {
                    Ok(asked) => asked,
                    Err(error) => failed("refused", error.to_string(), "try again shortly"),
                }
            }
            Asked::Webhooks(action) => forwarded_webhooks(&key, action).await,
        }
    }

    /// What a device that serves its person's account answers `asked` with as the key `grant` was
    /// made with: `None` for what its desktop answers, which [`here_answers`] does.
    async fn answer_here(
        &self,
        here: &dyn crate::session::Here,
        grant: &Grant,
        asked: &Asked,
        id: InvocationId,
    ) -> Option<Outcome> {
        if matches!(
            asked,
            Asked::Workspaces(
                WorkspaceAction::Enter { .. }
                    | WorkspaceAction::Leave { .. }
                    | WorkspaceAction::Join { .. }
            )
        ) {
            return None;
        }
        let host = here.account_here()?;
        let Some(key) = grant.key() else {
            return Some(refused(&ServerError::NoAccountHere));
        };
        Some(
            self.serve_here(here, host.as_ref(), key, grant, asked, id)
                .await,
        )
    }

    /// Answer `asked`, decoded from its declaration, for a device's own account served by its
    /// daemon.
    #[allow(
        clippy::too_many_lines,
        reason = "one match over the account's actions, each a few lines of an answer"
    )]
    async fn serve_here(
        &self,
        here: &dyn crate::session::Here,
        host: &dyn crate::session::AccountHere,
        key: GrantKey,
        grant: &Grant,
        asked: &Asked,
        id: InvocationId,
    ) -> Outcome {
        let Some(server) = self.0.server.upgrade() else {
            return failed("unavailable", "the device is stopping", "try again shortly");
        };
        match asked {
            Asked::Revoke(revoked) => {
                revoke(&server, grant, revoked, Recorder::Here(host, &key), false).await
            }
            Asked::Workspaces(
                WorkspaceAction::Enter { .. }
                | WorkspaceAction::Leave { .. }
                | WorkspaceAction::Join { .. },
            ) => failed(
                "unavailable",
                "the desktop that keeps the workspace answers this",
                "ask it on that device",
            ),
            Asked::Workspaces(WorkspaceAction::List { labels }) => {
                Self::here_workspaces(host, &key, grant, labels.as_ref())
            }
            Asked::Workspaces(WorkspaceAction::Create {
                name,
                labels,
                id: chosen,
            }) => {
                let (id, labels) =
                    match (chosen_id(chosen.as_deref()), labels_from(labels.as_ref())) {
                        (Ok(id), Ok(labels)) => (id, labels),
                        (Err(refused), _) | (_, Err(refused)) => return refused,
                    };
                match host.create(key, name.trim().to_owned(), labels, id).await {
                    Ok(workspace) => Outcome::Ok(json!({"workspace": workspace})),
                    Err(error) => refused(&error),
                }
            }
            Asked::Workspaces(WorkspaceAction::Branch {
                workspace,
                name,
                labels,
                id: chosen,
            }) => {
                let (id, labels) =
                    match (chosen_id(chosen.as_deref()), labels_from(labels.as_ref())) {
                        (Ok(id), Ok(labels)) => (id, labels),
                        (Err(refused), _) | (_, Err(refused)) => return refused,
                    };
                match host
                    .branch(
                        key,
                        workspace.trim().to_owned(),
                        name.trim().to_owned(),
                        labels,
                        id,
                    )
                    .await
                {
                    Ok(branch) => Outcome::Ok(json!({"workspace": branch})),
                    Err(error) => not_branched(&error).unwrap_or_else(|| refused(&error)),
                }
            }
            Asked::Workspaces(WorkspaceAction::Rename { workspace, name }) => {
                match host
                    .rename(key, workspace.trim().to_owned(), name.trim().to_owned())
                    .await
                {
                    Ok(workspace) => Outcome::Ok(json!({"workspace": workspace})),
                    Err(error) => refused(&error),
                }
            }
            Asked::Workspaces(WorkspaceAction::Delete { workspace }) => {
                let workspace = workspace.trim();
                match host.delete(key, workspace.to_owned()).await {
                    Ok(()) => {
                        // Every key that reached only that workspace goes with it.
                        let revoked = server.revoke_keys_for(&grant.person, workspace).await;
                        Outcome::Ok(json!({"deleted": workspace, "revoked_keys": revoked}))
                    }
                    Err(error) => refused(&error),
                }
            }
            Asked::Workspaces(WorkspaceAction::Unattended { workspace, on }) => {
                let workspace = workspace.trim();
                match host.unattended(key, workspace.to_owned(), *on).await {
                    Ok(on) => Outcome::Ok(json!({"workspace": workspace, "unattended": on})),
                    Err(error) => refused(&error),
                }
            }
            Asked::Keys(KeyAction::Create {
                name,
                workspace,
                expires_in,
                browser_pages,
            }) => {
                Certifying::ask(
                    host,
                    key,
                    id,
                    name,
                    workspace.as_deref(),
                    *expires_in,
                    browser_pages.unwrap_or(true),
                )
                .await
            }
            Asked::Keys(KeyAction::Mint {
                workspace,
                name,
                expires_in,
            }) => {
                Self::here_mint(
                    here,
                    host,
                    &key,
                    grant,
                    &server,
                    workspace,
                    name.as_deref(),
                    *expires_in,
                )
                .await
            }
            Asked::Usage(UsageAction::Read) => failed(
                "unavailable",
                "a device keeps no plan or usage of its own to read",
                "read the account on the server that keeps the workspaces",
            ),
            Asked::Models(ModelAction::Ask { workspace, request }) => {
                let workspace = workspace.trim();
                let asked = InvokeRequest {
                    invocation: id,
                    ..InvokeRequest::plain(model_providers::models::ask(), request.clone())
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
            Asked::Webhooks(WebhookAction::List { workspace }) => {
                match host
                    .webhooks(
                        key,
                        workspace
                            .as_deref()
                            .map(str::trim)
                            .filter(|workspace| !workspace.is_empty())
                            .map(str::to_owned),
                    )
                    .await
                {
                    Ok(listed) => Outcome::Ok(listed),
                    Err(error) => refused(&error),
                }
            }
            Asked::Webhooks(WebhookAction::Create {
                workspace,
                url,
                events,
                path,
            }) => {
                if let Some(why) = tilo_net::address::webhook_refusal(url) {
                    return failed(
                        "invalid_arguments",
                        why,
                        "name an https URL on the public internet, or an address of this computer",
                    );
                }
                let events = events.clone().unwrap_or_default();
                match host
                    .create_webhook(
                        key,
                        workspace.trim().to_owned(),
                        url.trim().to_owned(),
                        events,
                        path.as_deref().map(str::trim).map(str::to_owned),
                    )
                    .await
                {
                    Ok(made) => Outcome::Ok(made),
                    Err(error) => refused(&error),
                }
            }
            Asked::Webhooks(WebhookAction::Delete { webhook }) => {
                match host.delete_webhook(key, webhook.trim().to_owned()).await {
                    Ok(deleted) => Outcome::Ok(deleted),
                    Err(error) => refused(&error),
                }
            }
        }
    }

    /// The workspaces `key` reaches here, ones labelled with every one of `labels`.
    fn here_workspaces(
        host: &dyn crate::session::AccountHere,
        key: &GrantKey,
        grant: &Grant,
        labels: Option<&Map<String, Value>>,
    ) -> Outcome {
        let labels = match labels_from(labels) {
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
        workspace: &str,
        label: Option<&str>,
        expires_in: Option<u64>,
    ) -> Outcome {
        let id = match minted_scope(workspace) {
            Ok(id) => id,
            Err(outcome) => return outcome,
        };
        if !host
            .workspaces(key)
            .iter()
            .any(|reached| reached.id == workspace)
        {
            return refused(&ServerError::NotFound);
        }
        let (minted, answered) = match kept_mint(
            server,
            key,
            grant,
            (workspace, workspace),
            id,
            label,
            expires_in,
        ) {
            Ok(kept) => kept,
            Err(outcome) => return outcome,
        };
        // The workspace keeps the new key's certificate before anyone holds it, so its first
        // read is answered as that key's own.
        if let Some(key) = minted.key()
            && let Err(error) = here.enter(workspace, &key).await
        {
            tracing::warn!(%error, workspace, grant = %minted.id, "a minted key is not recorded in its workspace yet");
        }
        Outcome::Ok(answered)
    }

    /// Turn `workspace`'s unattended runs on or off as `asked` says, under the invocation `id`:
    /// on waits for the person to allow it.
    async fn unattended(
        &self,
        key: &Connection,
        id: InvocationId,
        workspace: &str,
        asked: &Asked,
    ) -> Outcome {
        let refusal = |why: String| {
            failed(
                "not_permitted",
                format!("the workspace refused: {why}"),
                "only the workspace's owner, from a key of theirs reaching it, has it; \
                 turning it on waits for them to allow it",
            )
        };
        match self
            .ask_host(key, workspace, id, asked, Some(ANSWERING_MS))
            .await
        {
            Ok(Outcome::Failed { message, .. }) => refusal(message),
            Ok(Outcome::Asked(actions)) => Outcome::Held(actions),
            Ok(asked) => asked,
            Err(error) => refusal(error.to_string()),
        }
    }

    /// Ask `workspace`'s host for what `asked` asks of it ([`account_target`]) as the key's
    /// person, under `id`: done, followed there, or failed.
    async fn ask_host(
        &self,
        key: &Connection,
        workspace: &str,
        id: InvocationId,
        asked: &Asked,
        wait_ms: Option<u32>,
    ) -> Result<Outcome, ActionCallError> {
        let (target, arguments) = account_target(asked);
        let entered = match key.server().enter(key, workspace.trim()).await {
            Ok(entered) => entered,
            Err(error) => return Ok(refused(&error)),
        };
        let actions = key.device.actions(entered);
        let request = InvokeRequest {
            invocation: id,
            wait_ms,
            ..InvokeRequest::plain(target, arguments)
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

    /// Make one of the key's person's workspaces, or branch one, as `op` and `args` say, under
    /// `id`: the workspace, at once or once the person allowed it.
    async fn make(&self, key: Arc<Connection>, asked: &Asked, id: InvocationId) -> Outcome {
        let (action, arguments) = match server_operation(asked) {
            Ok(asked) => asked,
            Err(outcome) => return outcome,
        };
        let made = key.server().make(&key, action, arguments, id).await;
        let answered = |made: Result<crate::server::Workspace, ServerError>| match made {
            Ok(made) => Outcome::Ok(json!({"workspace": made})),
            Err(error) => not_branched(&error).unwrap_or_else(|| refused(&error)),
        };
        match made {
            Err(ServerError::AwaitingApproval(_, awaiting)) => {
                let ask = awaiting.ask.clone();
                let account = self.clone();
                Outcome::Awaiting(
                    ask,
                    Box::pin(async move {
                        answered(
                            account
                                .decided(&key, *awaiting, id)
                                .await
                                .and_then(|output| crate::server::Server::made(&key, output)),
                        )
                    }),
                )
            }
            made => answered(made),
        }
    }

    /// What the server answered the operation `awaiting` once the person decided, as `key`
    /// asked it: followed while it waits for them and the account still keeps the invocation
    /// `id` it answers.
    async fn decided(
        &self,
        key: &Connection,
        mut awaiting: tilo_session_client::device::Awaiting,
        id: InvocationId,
    ) -> Result<Value, ServerError> {
        loop {
            match key.device.answer_of(&awaiting, DECIDING).await {
                Err(tilo_session_client::device::DeviceRefusal {
                    awaiting: Some(still),
                    ..
                }) if self.knows(&self.0.caller, &id) => awaiting = still,
                answered => return answered.map_err(crate::server::not_yours_not_found),
            }
        }
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
                    value: Some(crate::keys::approved(&token, &grant)),
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

/// What the grants answer `grant` for `asked`: a key for all the person's workspaces reaches
/// all of the account; a key for one reaches its own keys (a revocation, which its own key or its
/// maker takes back), and its workspace's setting and model.
fn answer_for(grant: &Grant, asked: &Asked) -> Answer {
    // A minted key is a workspace's agent, narrower than the key that made it: what only the
    // person does, it never does.
    let person_only = matches!(
        asked,
        Asked::Workspaces(
            WorkspaceAction::Create { .. }
                | WorkspaceAction::Branch { .. }
                | WorkspaceAction::Rename { .. }
                | WorkspaceAction::Delete { .. }
                | WorkspaceAction::Unattended { .. }
        ) | Asked::Keys(KeyAction::Create { .. } | KeyAction::Mint { .. })
    );
    if matches!(grant.kind, GrantKind::Minted { .. }) && person_only {
        return Answer::Deny;
    }
    match &grant.scope {
        Scope::Account => Answer::Allow,
        Scope::Workspace(own) => match asked {
            // Its list is the one workspace it reaches, as the server lists it for the key.
            Asked::Workspaces(WorkspaceAction::List { .. })
            | Asked::Webhooks(WebhookAction::List { .. } | WebhookAction::Delete { .. })
            | Asked::Revoke(_) => Answer::Allow,
            action @ (Asked::Workspaces(WorkspaceAction::Unattended { .. })
            | Asked::Models(_)
            | Asked::Webhooks(WebhookAction::Create { .. }))
                if action.workspace() == Some(own.as_str()) =>
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
            Run::Ended(state) | Run::Awaiting(state) => Some(state.clone()),
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
            Run::Certifying(certifying) => {
                let (answers, runs_as) =
                    (Arc::clone(&certifying.answers), certifying.runs_as.clone());
                drop(running);
                let state = answers.observe(&runs_as, id)?;
                if !state.update.is_terminal() {
                    return Some(state);
                }
                let Some(Run::Certifying(certifying)) =
                    crate::lock(&self.0.running).by_id.remove(id)
                else {
                    return self.observe(caller, id);
                };
                let ended = certifying.ended(*id, state);
                self.keep(*id, Run::Ended(ended.clone()));
                Some(ended)
            }
        }
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if let Some(change) = self.handed_on(|invocations| invocations.next_change(caller, id)) {
            return Some(change);
        }
        let running = crate::lock(&self.0.running);
        let Some(Run::Certifying(certifying)) = running.by_id.get(id) else {
            return None;
        };
        let (answers, runs_as) = (Arc::clone(&certifying.answers), certifying.runs_as.clone());
        drop(running);
        answers.next_change(&runs_as, id)
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

/// A key the person is asked for where their account is answered: that device keeps the ask in
/// their own workspace, as every question that waits for them is kept, until they answer it on a
/// device of theirs, and its root certifies the key once they approve. Its secret stays here.
pub(crate) struct Certifying {
    secret: ed25519_dalek::SigningKey,
    asked: crate::keys::KeyAsked,
    /// Where the ask is followed, and as whom.
    answers: Provider,
    runs_as: Caller,
}

impl Certifying {
    /// Ask `key`'s person, through `host`, for the key `name` asks for, under the invocation `id`.
    async fn ask(
        host: &dyn crate::session::AccountHere,
        key: GrantKey,
        id: InvocationId,
        name: &str,
        workspace: Option<&str>,
        expires_in: Option<u64>,
        browser_pages: bool,
    ) -> Outcome {
        let Ok(secret) = GrantKey::generate() else {
            return failed(
                "unavailable",
                "no randomness is available",
                "try again shortly",
            );
        };
        let asked = match crate::keys::KeyAsked::of(
            name,
            workspace,
            expires_in,
            browser_pages,
            secret.verifying_key().to_bytes(),
        ) {
            Ok(asked) => asked,
            Err(why) => {
                return failed(
                    "invalid_arguments",
                    why,
                    "pass {name, workspace?, expires_in?, browser_pages?}",
                );
            }
        };
        let Ok(Value::Object(arguments)) = serde_json::to_value(&asked) else {
            return failed(
                "invalid_arguments",
                "the key's ask could not be written",
                "pass {name, workspace?, expires_in?, browser_pages?}",
            );
        };
        let request = InvokeRequest {
            invocation: id,
            ..InvokeRequest::plain(
                RemoteActionTarget {
                    provider: crate::keys::CERTIFY_PROVIDER.to_owned(),
                    address: ActionAddress::new(
                        crate::keys::CERTIFY_RESOURCE,
                        crate::keys::CERTIFY,
                    ),
                },
                arguments,
            )
        };
        let own = tilo_server_protocol::servers::person_workspace_id(&key.cert().user_public_key);
        let Some((receipt, answers, runs_as)) = host.ask(key, own, request).await else {
            return failed(
                "unavailable",
                "the person's own workspace is not open on the device that answers for their account",
                "try again shortly",
            );
        };
        // The ask always waits for the person: anything else is the device refusing it.
        match receipt.status {
            ReceiptStatus::Accepted => Outcome::Certifying(Box::new(Self {
                secret,
                asked,
                answers,
                runs_as,
            })),
            refused => {
                let why = match refused {
                    ReceiptStatus::Failed { error } => error.message,
                    ReceiptStatus::Rejected { error } => error.to_string(),
                    _ => "the device that answers for the account did not ask the person".into(),
                };
                failed("refused", why, "ask again with account/keys#create")
            }
        }
    }

    /// How the key ended, as the ask ended in `state`: made of the certificate the person's
    /// root gave it, declined, or refused.
    fn ended(self, id: InvocationId, state: InvocationState) -> InvocationState {
        let ended = |code: &str, message: String, next: &str| InvocationUpdate::Failed {
            invocation: id,
            error: failure(code, message, next),
        };
        let update = match state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                let cert = output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("cert"))
                    .and_then(Value::as_str)
                    .and_then(|cert| hex::decode(cert).ok())
                    .and_then(|bytes| {
                        tilo_user_identity::canonical::from_canonical_bytes::<
                            tilo_user_identity::DeviceCert,
                        >(&bytes)
                        .ok()
                    });
                match cert.map(|cert| self.asked.made(self.secret, cert)) {
                    Some(Ok(value)) => InvocationUpdate::Succeeded {
                        invocation: id,
                        output: ActionOutput {
                            value: Some(value),
                            ..ActionOutput::default()
                        },
                    },
                    Some(Err(why)) => ended("refused", why, "ask again with account/keys#create"),
                    None => ended(
                        "refused",
                        "the person's root gave the key no certificate".to_owned(),
                        "ask again with account/keys#create",
                    ),
                }
            }
            InvocationUpdate::Cancelled { message, .. }
                if message.as_deref() == Some(tilo_actions::remote::asked::DECLINED) =>
            {
                ended(
                    "declined",
                    "the person declined the key".to_owned(),
                    "ask again only if they want it",
                )
            }
            update => update,
        };
        InvocationState::new(id, update)
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

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.actions.as_ref()?.change_feed()
    }

    /// What the device says of the actions a window shows: answered by the actions this tree
    /// joins, which decide each as the person at the window.
    fn shown_actions(
        &self,
        caller: &Caller,
        ask: tilo_actions::shown::ShownAsk,
    ) -> Pending<Result<Vec<tilo_actions::shown::ActionDecision>, ActionCallError>> {
        match &self.actions {
            Some(actions) => actions.shown_actions(caller, ask),
            None => Pending::ready(Err(ActionCallError::Unavailable {
                reason: tilo_actions::shown::UNAVAILABLE.into(),
            })),
        }
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
    /// Delete `workspace`, and every key that reached only it.
    async fn delete_workspace(
        self: &Arc<Self>,
        account: &Account,
        (grant, key): (&Grant, &Arc<Connection>),
        (id, workspace): (InvocationId, &str),
    ) -> Outcome {
        match key.server().delete(key, workspace, id).await {
            Ok(()) => {}
            // The person is asked: deleted, and its keys revoked, once they allow it.
            Err(ServerError::AwaitingApproval(_, awaiting)) => {
                let ask = awaiting.ask.clone();
                let (server, account, key) = (Arc::clone(self), account.clone(), Arc::clone(key));
                let (person, workspace) = (grant.person.clone(), workspace.to_owned());
                return Outcome::Awaiting(
                    ask,
                    Box::pin(async move {
                        if let Err(error) = account.decided(&key, *awaiting, id).await {
                            return refused(&error);
                        }
                        crate::server::Server::deleted(&key, &workspace);
                        server.workspace_deleted(&person, &workspace).await
                    }),
                );
            }
            Err(error) => return refused(&error),
        }
        self.workspace_deleted(&grant.person, workspace).await
    }

    /// `person`'s `workspace` was deleted: every key that reached only that workspace goes with
    /// it.
    async fn workspace_deleted(&self, person: &str, workspace: &str) -> Outcome {
        let revoked = self.revoke_keys_for(person, workspace).await;
        Outcome::Ok(json!({"deleted": workspace, "revoked_keys": revoked}))
    }

    /// Ask for the key `name` asks for, for the person to approve.
    fn create_key(
        &self,
        name: &str,
        workspace: Option<&str>,
        expires_in: Option<u64>,
        browser_pages: bool,
    ) -> Outcome {
        match self.request_key(Some(name), workspace, expires_in, browser_pages) {
            Ok((poll, _)) => Outcome::Key(poll),
            Err(why) => failed(
                "invalid_arguments",
                why,
                "pass {name, workspace?, expires_in?, browser_pages?}",
            ),
        }
    }

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

    /// Mint a key reaching `workspace`, for an agent to act there.
    async fn mint(
        self: &Arc<Self>,
        grant: &Grant,
        connection: &Connection,
        workspace: &str,
        label: Option<&str>,
        expires_in: Option<u64>,
    ) -> Outcome {
        let id = match minted_scope(workspace) {
            Ok(id) => id,
            Err(outcome) => return outcome,
        };
        let found = match connection.server().workspace(connection, workspace).await {
            Ok(found) => found,
            Err(error) => return refused(&error),
        };
        let Some(found) = found else {
            return refused(&ServerError::NotFound);
        };
        let (minted, answered) = match kept_mint(
            self,
            &connection.key,
            grant,
            (workspace, &found.name),
            id,
            label,
            expires_in,
        ) {
            Ok(kept) => kept,
            Err(outcome) => return outcome,
        };
        // The workspace keeps the new key's certificate before anyone holds it, so its first
        // read is answered as that key's own.
        self.admit(&minted).await;
        Outcome::Ok(answered)
    }
}

/// How a key's revocation reaches the workspaces that admitted it: a device's own account, asked
/// of its host as the key, or a server's, each workspace asked over the key's connection.
enum Recorder<'a> {
    Here(&'a dyn crate::session::AccountHere, &'a GrantKey),
    Server(&'a Connection),
}

impl Recorder<'_> {
    /// The workspaces the key may have been admitted into.
    async fn workspaces(&self) -> Result<Vec<String>, Outcome> {
        match self {
            Self::Here(host, key) => Ok(host
                .workspaces(key)
                .into_iter()
                .map(|workspace| workspace.id)
                .collect()),
            Self::Server(connection) => connection
                .server()
                .labelled(connection, &WorkspaceLabels::new())
                .await
                .map(|workspaces| {
                    workspaces
                        .into_iter()
                        .map(|workspace| workspace.id)
                        .collect()
                })
                .map_err(|error| refused(&error)),
        }
    }

    /// How `workspace` recorded the revocation of `id`: whether it changed.
    async fn revoke(&self, workspace: &str, id: &str) -> Result<bool, Outcome> {
        match self {
            Self::Here(host, key) => host
                .revoke_key((*key).clone(), workspace.to_owned(), id.to_owned())
                .await
                .map_err(|error| refused(&error)),
            Self::Server(connection) => revoke_in(connection, workspace, id).await,
        }
    }
}

/// Revoke the key `id`, and have every workspace that admitted it record it: a workspace that
/// does not is said, and `fails` tells whether the call fails with it or answers anyway.
async fn revoke(
    server: &Server,
    grant: &Grant,
    id: &str,
    recorder: Recorder<'_>,
    fails: bool,
) -> Outcome {
    let kept_here = server
        .store
        .grant(id)
        .filter(|found| found.person == grant.person);
    let minted = server
        .store
        .grant(id)
        .is_some_and(|found| found.parent() == Some(grant.id.as_str()));
    // A key of the person's own reaching all their workspaces revokes any key of theirs,
    // wherever it was made: each workspace that lists it keeps the revocation.
    if grant.scope == Scope::Account && persons_own(grant) && id != grant.id && !minted {
        let workspaces = match recorder.workspaces().await {
            Ok(workspaces) => workspaces,
            Err(outcome) => return outcome,
        };
        let mut kept = false;
        for workspace in workspaces {
            match recorder.revoke(&workspace, id).await {
                Ok(changed) => kept |= changed,
                Err(outcome) => {
                    tracing::warn!(
                        workspace,
                        grant = id,
                        "a workspace did not keep a key's revocation"
                    );
                    if fails {
                        return outcome;
                    }
                }
            }
        }
        if kept_here.is_none() && !kept {
            return failed(
                "not_found",
                "no such key of yours",
                "tilo_ls record:tilo.grants lists them in each workspace",
            );
        }
        if kept_here.is_some()
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
    if id != grant.id && !minted {
        return failed(
            "not_found",
            "no such key of this key",
            "tilo_ls record:tilo.grants lists them in each workspace",
        );
    }
    // Where the key is recorded: the workspace it reaches, or the calling key's own.
    let workspace = match server
        .store
        .grant(id)
        .map_or_else(|| grant.scope.clone(), |found| found.scope)
    {
        Scope::Workspace(workspace) => Some(workspace),
        Scope::Account => None,
    };
    if server.revoke_grant(&grant.person, id).await.is_err() {
        return failed(
            "unavailable",
            "revocation is unavailable",
            "try again shortly",
        );
    }
    // The workspace keeps the revocation, so every device and API server refuses the key.
    if let Some(workspace) = workspace
        && let Err(outcome) = recorder.revoke(&workspace, id).await
    {
        tracing::warn!(
            workspace,
            grant = id,
            "the workspace did not record a key's revocation"
        );
        if fails {
            return outcome;
        }
    }
    Outcome::Ok(json!({"revoked": id}))
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
    // Recorded as the API server itself asks, with no key behind the call.
    let caller = Caller::local_person("tilo api");
    match workspace_verb_as(connection, workspace, REVOKE_KEY, arguments, &caller).await {
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
async fn forwarded_webhooks(connection: &Connection, action: &WebhookAction) -> Outcome {
    let cert = connection.key.cert();
    let caller = tilo_peer_state::key_caller::caller_of_certificate(cert);
    let named = match action {
        WebhookAction::List { workspace } => workspace
            .as_deref()
            .map(str::trim)
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_owned),
        WebhookAction::Create { workspace, .. } => Some(workspace.trim().to_owned()),
        WebhookAction::Delete { .. } => None,
    };
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
    match action {
        WebhookAction::List { .. } => {
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
        WebhookAction::Create {
            workspace,
            url,
            events,
            path,
        } => {
            if let Some(why) = tilo_net::address::webhook_refusal(url) {
                return failed(
                    "invalid_arguments",
                    why,
                    "name an https URL on the public internet, or an address of this computer",
                );
            }
            let mut arguments = Map::new();
            arguments.insert("url".into(), json!(url));
            if let Some(events) = events {
                arguments.insert("events".into(), json!(events));
            }
            if let Some(path) = path {
                arguments.insert("path".into(), json!(path));
            }
            match workspace_verb_as(connection, workspace, CREATE_WEBHOOK, arguments, &caller).await
            {
                Ok(made) => Outcome::Ok(made),
                Err(outcome) => outcome,
            }
        }
        WebhookAction::Delete { webhook } => {
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
    let request = InvokeRequest::plain(
        RemoteActionTarget::workspace_governance(workspace, action),
        arguments,
    );
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
fn host_rows(server: &Server) -> Vec<actions::HostRow> {
    server
        .hosts
        .names()
        .into_iter()
        .enumerate()
        .map(|(index, name)| actions::HostRow {
            name,
            default: index == 0,
        })
        .collect()
}

/// Whether `workspace` is labelled with every one of `labels`.
pub(crate) fn labelled(workspace: &crate::server::Workspace, labels: &WorkspaceLabels) -> bool {
    labels
        .iter()
        .all(|(name, value)| workspace.labels.get(name) == Some(value))
}

/// The workspaces `key` reaches that are labelled with every one of the `labels` `args` names,
/// with the hosts it may create on.
async fn workspaces(
    key: &Connection,
    server: &Server,
    labels: Option<&Map<String, Value>>,
) -> Outcome {
    let labels = match labels_from(labels) {
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
/// What it answers a create with when that device asked the person first and they said no.
pub const FOUNDING_DECLINED: &str = "founding_declined";
/// What it answers a create with when that device asked the person first and nobody answered.
pub const FOUNDING_UNANSWERED: &str = "founding_unanswered";

/// Make the workspace `name` asks for on the person's device that founds their workspaces, as
/// [`crate::session::Here::create_where_founded`] asks it: its answer as that device gave it.
async fn create_where_founded(
    here: &dyn Here,
    name: &str,
    labels: Option<&Map<String, Value>>,
    id: Option<&str>,
) -> Outcome {
    let (id, labels) = match (chosen_id(id), labels_from(labels)) {
        (Ok(id), Ok(labels)) => (id, labels),
        (Err(refused), _) | (_, Err(refused)) => return refused,
    };
    let creating = crate::session::Creating {
        name: name.trim().to_owned(),
        labels,
        id,
    };
    let why = match here.create_where_founded(creating).await {
        Ok(answer) => return Outcome::Ok(answer),
        Err(why) => why,
    };
    let (code, hint) = match &why {
        crate::session::NotFounded::NoFoundingDevice => (
            NO_FOUNDING_DEVICE,
            "make the workspace on the computer you first set Tilo up on",
        ),
        crate::session::NotFounded::Unreached { .. } => (
            FOUNDING_DEVICE_UNREACHED,
            "turn that device on and connect it to a network, then try again",
        ),
        crate::session::NotFounded::Declined => {
            (FOUNDING_DECLINED, "make it again and say yes there")
        }
        crate::session::NotFounded::Unanswered { .. } => (
            FOUNDING_UNANSWERED,
            "make it again and answer the question on that device",
        ),
        crate::session::NotFounded::Refused(_) => ("refused", "fix what it names and try again"),
    };
    failed(code, why.to_string(), hint)
}

/// What a desktop answers `op` with `args`: the workspaces it keeps, kept or left at the
/// person's word, and everything else but making one as the person's account at their server
/// answers it. A workspace branched there is kept here too, and one deleted there is kept here
/// no more.
/// Join `workspace` through its open entry at `host`, as its directory publishes the host: the
/// device signs its own admission, and the entry's refusal is said in its own words.
async fn join_entry(
    here: &dyn Here,
    workspace: &str,
    host: Option<&Map<String, Value>>,
) -> Outcome {
    let Some(host) = host else {
        return failed(
            "invalid_arguments",
            "`host` is required with `workspace`",
            "pass the host the workspace's directory publishes: {hostCert, address}",
        );
    };
    let host = match serde_json::from_value::<tilo_server_protocol::web::PageHost>(Value::Object(
        host.clone(),
    )) {
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

async fn here_answers(
    here: &dyn Here,
    key: &tilo_user_identity::DeviceCert,
    server: &Server,
    asked: &Asked,
) -> Outcome {
    match asked {
        Asked::Workspaces(WorkspaceAction::List { labels }) => {
            return listed_here(here, key, server, labels.as_ref()).await;
        }
        Asked::Workspaces(WorkspaceAction::Enter { workspace }) => {
            let workspace = workspace.trim();
            return match here.keep(workspace).await {
                Ok(()) => Outcome::Ok(json!({"workspace": workspace, "kept_here": true})),
                Err(why) => failed("unavailable", why, "try again shortly"),
            };
        }
        Asked::Workspaces(WorkspaceAction::Leave { workspace }) => {
            let workspace = workspace.trim();
            return match here.leave(workspace).await {
                Ok(()) => Outcome::Ok(json!({"workspace": workspace, "kept_here": false})),
                Err(why) => failed("unavailable", why, "try again shortly"),
            };
        }
        Asked::Workspaces(WorkspaceAction::Join {
            workspace,
            invite,
            host: at,
        }) => {
            if let Some(workspace) = workspace.as_deref() {
                return join_entry(here, workspace.trim(), at.as_ref()).await;
            }
            let Some(invite) = invite.as_deref().map(str::trim) else {
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
        Asked::Webhooks(_) => {
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
    if let Asked::Workspaces(WorkspaceAction::Create { name, labels, id }) = asked {
        return create_where_founded(here, name, labels.as_ref(), id.as_deref()).await;
    }
    let Some(account) = here.account().await else {
        return refused(&ServerError::NoAccountHere);
    };
    let (target, arguments) = match asked {
        Asked::Workspaces(WorkspaceAction::Rename { .. } | WorkspaceAction::Unattended { .. })
        | Asked::Models(_)
        | Asked::Usage(_) => account_target(asked),
        _ => match server_operation(asked) {
            Ok((action, arguments)) => (host(action), arguments),
            Err(outcome) => return outcome,
        },
    };
    let answer = match account.call(target, arguments).await {
        Ok(answer) => answer,
        Err(why) => return failed("unavailable", why, "try again shortly"),
    };
    match asked {
        Asked::Workspaces(WorkspaceAction::Delete { workspace }) => {
            if here
                .workspaces(key)
                .iter()
                .any(|kept| &kept.id == workspace)
                && let Err(why) = here.leave(workspace).await
            {
                tracing::warn!(%why, %workspace, "a deleted workspace's copy is still kept here");
            }
            Outcome::Ok(json!({"deleted": workspace}))
        }
        Asked::Workspaces(WorkspaceAction::Branch { .. }) => kept_made(here, answer).await,
        _ => Outcome::Ok(answer),
    }
}

/// A workspace the server made from this device, as `answer` names it, kept here too.
async fn kept_made(here: &dyn Here, answer: Value) -> Outcome {
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

/// What a computer asks its server for `asked` beyond the person's workspaces themselves, as a
/// key asks it over its connection: renaming a workspace and turning its unattended runs on or
/// off as the workspace itself decides, the account's usage, and a turn of the person's model.
fn account_target(asked: &Asked) -> (RemoteActionTarget, Map<String, Value>) {
    let mut arguments = Map::new();
    let target = match asked {
        Asked::Workspaces(WorkspaceAction::Rename { workspace, name }) => {
            arguments.insert("name".into(), json!(name.trim()));
            RemoteActionTarget::workspace_governance(workspace.trim(), RENAME)
        }
        Asked::Workspaces(WorkspaceAction::Unattended { workspace, on }) => {
            arguments.insert("on".into(), json!(on));
            RemoteActionTarget::workspace_governance(workspace.trim(), RUN_UNATTENDED)
        }
        Asked::Models(ModelAction::Ask { request, .. }) => {
            arguments.clone_from(request);
            model_providers::models::ask()
        }
        _ => tilo_action_services::server_account::target(
            tilo_action_services::server_account::ACCOUNT,
        ),
    };
    (target, arguments)
}

/// The server's own operation that answers `op`, asked with `args`, and its arguments: making,
/// branching and deleting the person's workspaces. A computer asks its server nothing else
/// itself; a key asks the rest over its connection.
fn server_operation(asked: &Asked) -> Result<(&'static str, Map<String, Value>), Outcome> {
    let mut arguments = Map::new();
    let (action, name, labels, id) = match asked {
        Asked::Workspaces(WorkspaceAction::Create { name, labels, id }) => {
            (CREATE_OWN_WORKSPACE, Some(name.trim()), labels, id)
        }
        Asked::Workspaces(WorkspaceAction::Branch {
            workspace,
            name,
            labels,
            id,
        }) => {
            arguments.insert("workspace".into(), json!(workspace.trim()));
            (BRANCH_OWN_WORKSPACE, Some(name.trim()), labels, id)
        }
        Asked::Workspaces(WorkspaceAction::Delete { workspace }) => {
            arguments.insert("workspace".into(), json!(workspace));
            return Ok((DELETE_WORKSPACE, arguments));
        }
        _ => {
            return Err(failed(
                "not_permitted",
                "a computer's own keys are made on it with `tilo keys`, not asked of its server",
                "run `tilo keys` on this computer",
            ));
        }
    };
    if let Some(name) = name {
        arguments.insert("name".into(), json!(name));
    }
    let labels = labels_from(labels.as_ref())?;
    if !labels.is_empty() {
        arguments.insert("labels".into(), json!(labels));
    }
    let made = chosen_id(id.as_deref())?
        .unwrap_or_else(uuid::Uuid::new_v4)
        .to_string();
    // A create names the workspace it makes; a branch keeps its source and names what it makes.
    let named = if action == CREATE_OWN_WORKSPACE {
        "workspace"
    } else {
        "branch"
    };
    arguments.insert(named.into(), json!(made));
    Ok((action, arguments))
}

/// How long a device that holds workspaces waits on its server's list of the person's others.
const SERVER_LIST_BESIDE_HELD: std::time::Duration = std::time::Duration::from_secs(1);

/// The workspaces a desktop keeps, and those of the person's its server lists that it does
/// not keep, labelled with every one of the `labels` `args` names, with the hosts it may create on.
async fn listed_here(
    here: &dyn Here,
    key: &tilo_user_identity::DeviceCert,
    server: &Server,
    labels: Option<&Map<String, Value>>,
) -> Outcome {
    let labels = match labels_from(labels) {
        Ok(labels) => labels,
        Err(refused) => return refused,
    };
    let mut workspaces = here.workspaces(key);
    // The server's list adds the person's workspaces this device does not hold. A device that
    // holds some lists them at once whether or not its server answers: a host that hangs or a
    // network that loses its packets costs it a moment, never its list.
    let server_list = async {
        let account = here.account().await?;
        Some(account.listed(&key.user_public_key, &labels, None).await)
    };
    let server_list = if workspaces.is_empty() {
        server_list.await
    } else {
        tokio::time::timeout(SERVER_LIST_BESIDE_HELD, server_list)
            .await
            .unwrap_or_else(|_| Some(Err("the server did not answer in time".to_owned())))
    };
    if let Some(listed) = server_list {
        match listed {
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
    // One the list shows here that this device has not taken in yet says why, beside the rest.
    for missing in here.not_here(key) {
        match workspaces.iter_mut().find(|row| row.id == missing.id) {
            Some(row) if !row.kept_here => row.not_here = missing.not_here,
            Some(_) => {}
            None => workspaces.push(missing),
        }
    }
    workspaces.retain(|workspace| labelled(workspace, &labels));
    Outcome::Ok(json!({"workspaces": workspaces, "hosts": host_rows(server)}))
}

/// Name `workspace` `name`, as `key`: the server decides with the workspace's own actions, as for
/// the person's own page.
async fn rename_workspace(key: &Connection, workspace: &str, name: &str) -> Outcome {
    match key
        .server()
        .rename(key, workspace.trim(), name.trim())
        .await
    {
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

/// The labels a call names (`labels`, names to text values): none when it names none.
fn labels_from(labels: Option<&Map<String, Value>>) -> Result<WorkspaceLabels, Outcome> {
    let Some(labels) = labels else {
        return Ok(WorkspaceLabels::new());
    };
    let labels =
        serde_json::from_value::<WorkspaceLabels>(Value::Object(labels.clone())).map_err(|_| {
            failed(
                "invalid_arguments",
                "`labels` names text values: {\"customer_id\": \"cus_8121\"}",
                "pass labels as an object of names to text",
            )
        })?;
    tilo_server_protocol::workspaces::check_labels(&labels)
        .map_err(|why| failed("invalid_arguments", why, "fix the label and ask again"))?;
    Ok(labels)
}

/// The id a caller chose for a workspace it makes, so that a retry asks for the same one: none
/// when it chose none.
fn chosen_id(id: Option<&str>) -> Result<Option<uuid::Uuid>, Outcome> {
    id.map(str::trim)
        .filter(|id| !id.is_empty())
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

/// The workspace id a minted key reaches, as `account/keys#mint` names it.
fn minted_scope(workspace: &str) -> Result<[u8; 16], Outcome> {
    match AgentScope::parse(workspace) {
        Some(AgentScope::Workspace(id)) => Ok(id),
        _ => Err(failed(
            "invalid_arguments",
            "`workspace` is a workspace's id",
            "account/workspaces#list names them",
        )),
    }
}

/// Keep the key `id` mints for `workspace` (called `called`), named `label` or after the
/// minting key and the workspace, and answer `account/keys#mint` with it: the minted key, and
/// the answer the caller reads.
fn kept_mint(
    server: &Server,
    key: &GrantKey,
    grant: &Grant,
    (workspace, called): (&str, &str),
    id: [u8; 16],
    label: Option<&str>,
    expires_in: Option<u64>,
) -> Result<(Grant, Value), Outcome> {
    let name: String = label
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| format!("{}: {called}", grant.client_name), str::to_owned)
        .chars()
        .filter(|character| !character.is_control())
        .take(crate::keys::MAX_NAME_CHARS)
        .collect();
    let now = now_ms();
    let minted = mint_key(key, id, &name, expires_in, now)?;
    let kind = GrantKind::Minted {
        parent: grant.id.clone(),
    };
    let (minted, token) = server
        .store
        .resource_key(&minted, &name, kind, None, now)
        .map_err(|_| {
            failed(
                "unavailable",
                "the key could not be kept",
                "try again shortly",
            )
        })?;
    let answered = json!({
        "grant": minted.id,
        "name": minted.client_name,
        "token": token,
        "workspace": workspace,
        "mcp_url": format!("{}/w/{workspace}/mcp", server.config.issuer),
        "expires_at_ms": minted.expires_ms,
    });
    Ok((minted, answered))
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
