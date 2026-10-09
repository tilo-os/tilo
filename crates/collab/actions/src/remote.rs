//! Using another peer's actions: a transport-neutral vocabulary.
//!
//! A peer that hosts live apps (a desktop session, a headless app-serving
//! host) can let an authenticated remote caller list the actions it
//! publishes, invoke one, follow its progress, and cancel it. This module is
//! the whole contract between the two sides:
//!
//! - [`RemoteActionMessage`] — requests and their answers, wrapped in a
//!   versioned [`RemoteActionFrame`] that any transport carries as opaque
//!   bytes.
//! - [`ActionHost`] — what a peer implements to answer them, and [`serve`],
//!   which maps one request frame onto a host. Answers that need an
//!   application's help arrive later as a [`Pending`], so no caller blocks.
//! - [`CatalogueQuery`] — paged, filtered browsing of what a host offers.
//! - [`InvocationLedger`] — the idempotency rule every host shares: an
//!   invocation is keyed by its caller-chosen [`InvocationId`], so a retry
//!   after a lost reply observes the original invocation instead of running
//!   the action twice.
//!
//! Who is calling is never read from a payload. The transport that
//! authenticated the connection constructs the [`Caller`] and hands it to the
//! host beside the message; nothing in a frame can name or change it.
//!
//! Descriptors, addresses, arguments, outputs and lifecycle updates are the
//! same types the local action surface already uses ([`ActionDescriptor`](crate::door::ActionDescriptor),
//! [`ActionAddress`], [`InvocationUpdate`], [`ActionOutput`],
//! [`ActionFailure`]), so a remote caller sees exactly what a local one does.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// The shared view action that asks the platform to reveal a running window.
pub const APPLICATION_ACTIVATE_ACTION: &str = "activate-window";
/// Encoded operation of [`APPLICATION_ACTIVATE_ACTION`].
pub const APPLICATION_ACTIVATE_TYPE: &str = "tilo.app.activate-window";

mod catalogue;
mod pending;

pub use catalogue::{
    CanonicalActionRoute, CatalogueLimits, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, MAX_CATALOGUE_PAGE, ResourceViewLocator, RowVisibility,
    ViewResourceQuery,
};
pub use pending::{Abandoned, Completer, Pending, PendingPoll, pending};

use crate::context::ActionContext;
use crate::door::ActionAddress;
use crate::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use crate::params::ARGUMENTS_JSON_PREFIX;

/// Version of the [`RemoteActionFrame`] schema this build speaks.
///
/// Additive changes (a new optional field) keep the version; a change an
/// older peer would misread bumps it, and [`RemoteActionFrame::decode`]
/// refuses a version it does not know instead of guessing.
pub const REMOTE_ACTIONS_VERSION: u16 = 1;

/// Upper bound on one encoded frame. Catalogues are the largest messages;
/// a host that needs more pages them rather than raising this.
pub const MAX_REMOTE_ACTION_FRAME_BYTES: usize = 1024 * 1024;

/// Default number of invocations an [`InvocationLedger`] remembers.
pub const DEFAULT_LEDGER_CAPACITY: usize = 1024;

/// Who a request is from, as the host's policy knows them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CallerPrincipal {
    /// A person, by their policy principal id. For a remote device this is
    /// the hex encoding of the user root its certificate chains to, so two
    /// devices of one person are the same principal.
    Person(String),
    /// A delegated agent, by its agent id.
    Agent(String),
    /// An application, by its package id, acting under its own identity
    /// rather than borrowing the authority of the person using it.
    App(String),
}

/// Host-owned execution lifetime. Clones share closure, never authority.
/// Equality and hashing use the immutable minted identity, not liveness.
#[derive(Clone, Debug)]
pub struct ExecutionLease {
    id: uuid::Uuid,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl PartialEq for ExecutionLease {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ExecutionLease {}
impl std::hash::Hash for ExecutionLease {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}
impl ExecutionLease {
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Retire observation of this run. This does not cancel any app operation.
    pub fn close(&self) {
        self.live.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// The authenticated principal behind a request.
///
/// Built only by the transport that authenticated the connection — from the
/// certificate chain a peer presented, or from the admitted identity of a
/// local connection. It is deliberately not `Deserialize`: a payload cannot
/// assert who sent it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Caller {
    principal: CallerPrincipal,
    user_public_key: Option<[u8; 32]>,
    device_public_key: Option<[u8; 32]>,
    room: Option<String>,
    foreign: bool,
    // Transport-issued execution scope, never accepted from request context.
    // It narrows invocation ownership without changing the policy principal.
    execution: Option<ExecutionLease>,
    // The person a non-person principal acts for, as the host admitted it.
    acting_for: Option<String>,
    // The Shortcut whose run this person's invocation is a step of.
    shortcut: Option<String>,
}

impl Caller {
    /// A remote device whose certificate the transport verified to chain to
    /// `user_public_key`.
    #[must_use]
    pub fn authenticated(user_public_key: [u8; 32], device_public_key: [u8; 32]) -> Self {
        Self {
            principal: CallerPrincipal::Person(hex(&user_public_key)),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            room: None,
            foreign: false,
            execution: None,
            acting_for: None,
            shortcut: None,
        }
    }

    /// A person on a connection the host admitted itself, by the policy
    /// principal id that admission established.
    #[must_use]
    pub fn local_person(principal: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::Person(principal.into()),
            user_public_key: None,
            device_public_key: None,
            room: None,
            foreign: false,
            execution: None,
            acting_for: None,
            shortcut: None,
        }
    }

    /// A delegated agent on a connection the host admitted itself.
    #[must_use]
    pub fn local_agent(agent_id: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::Agent(agent_id.into()),
            user_public_key: None,
            device_public_key: None,
            room: None,
            foreign: false,
            execution: None,
            acting_for: None,
            shortcut: None,
        }
    }

    /// An application on a connection the host admitted itself, by the
    /// package id that admission established. It holds no role of its own:
    /// it may use only what it owns and what a workspace grants it.
    #[must_use]
    pub fn local_app(app_id: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::App(app_id.into()),
            user_public_key: None,
            device_public_key: None,
            room: None,
            foreign: false,
            execution: None,
            acting_for: None,
            shortcut: None,
        }
    }

    /// A remote agent device whose certificate the transport verified to
    /// chain to `user_public_key` and whose root-signed delegation names it
    /// agent `agent_id`. It is its own principal, never the person who
    /// delegated it.
    #[must_use]
    pub fn delegated_agent(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self {
            principal: CallerPrincipal::Agent(agent_id.into()),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            room: None,
            foreign: false,
            execution: None,
            acting_for: None,
            shortcut: None,
        }
    }

    /// Give an already admitted connection its own execution scope. The host
    /// mints the identifier; no payload, display name or history run selects it.
    /// This changes invocation ownership, never the caller's permissions.
    #[must_use]
    pub fn in_new_execution(mut self) -> Self {
        self.execution = Some(ExecutionLease {
            id: uuid::Uuid::new_v4(),
            live: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        self
    }

    /// Host-issued execution identity. An identifier alone conveys no authority.
    #[must_use]
    pub fn execution_id(&self) -> Option<uuid::Uuid> {
        self.execution.as_ref().map(|lease| lease.id)
    }

    #[must_use]
    pub fn execution_lease(&self) -> Option<ExecutionLease> {
        self.execution.clone()
    }

    #[must_use]
    pub fn execution_is_live(&self) -> bool {
        self.execution.as_ref().is_some_and(ExecutionLease::is_live)
    }

    /// A transport ends its own run; retained caller clones cannot reopen it.
    pub fn retire_execution(&self) {
        if let Some(lease) = &self.execution {
            lease.close();
        }
    }

    /// Mark whether the caller acts from a foreign device: a Tilo runtime on
    /// an operating system Tilo does not control, as its certificate says.
    /// Workspaces it does not govern cap such a caller's role.
    #[must_use]
    pub fn from_foreign_device(mut self, foreign: bool) -> Self {
        self.foreign = foreign;
        self
    }

    /// The caller acts for `person`, by that person's policy principal id:
    /// an agent, a Shortcut or an app the person started. Set only by the
    /// host that admitted the delegation, never from a payload. It names who
    /// the actor works for in presence and history; it widens nothing.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<String>) -> Self {
        self.acting_for = Some(person.into());
        self
    }

    /// The person this caller acts for, when it is not that person.
    #[must_use]
    pub fn acting_for_person(&self) -> Option<&str> {
        self.acting_for.as_deref()
    }

    /// The caller's invocations are the steps of Shortcut `id`'s run: still
    /// the person's authority (bounded by the Shortcut's own grants, which
    /// the runner applies), but the Shortcut is who acts, for the person.
    /// Set only by the host's Shortcut runner.
    #[must_use]
    pub fn running_shortcut(mut self, id: impl Into<String>) -> Self {
        self.shortcut = Some(id.into());
        self
    }

    /// The Shortcut whose run this invocation is a step of.
    #[must_use]
    pub fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }

    /// Who acts, as presence and history name them: the kind of actor, its
    /// id, and the person it acts for when that is someone else.
    #[must_use]
    pub fn actor(&self) -> crate::InteractionActor {
        match (&self.principal, &self.shortcut) {
            (CallerPrincipal::Person(person), Some(shortcut)) => crate::InteractionActor::new(
                format!("shortcut:{shortcut}"),
                crate::ActorKind::Shortcut,
            )
            .acting_for(person.clone()),
            (CallerPrincipal::Person(person), None) => {
                crate::InteractionActor::new(person.clone(), crate::ActorKind::Person)
            }
            (CallerPrincipal::Agent(agent), _) => {
                let actor =
                    crate::InteractionActor::new(format!("agent:{agent}"), crate::ActorKind::Agent);
                match &self.acting_for {
                    Some(person) => actor.acting_for(person.clone()),
                    None => actor,
                }
            }
            (CallerPrincipal::App(app), _) => {
                let actor =
                    crate::InteractionActor::new(format!("app:{app}"), crate::ActorKind::App);
                match &self.acting_for {
                    Some(person) => actor.acting_for(person.clone()),
                    None => actor,
                }
            }
        }
    }

    /// Confine the caller to one room: the connection was admitted to that
    /// room only, so nothing outside it is offered or reachable.
    #[must_use]
    pub fn in_room(mut self, room: impl Into<String>) -> Self {
        self.room = Some(room.into());
        self
    }

    #[must_use]
    pub const fn principal(&self) -> &CallerPrincipal {
        &self.principal
    }

    /// The room this caller is confined to, if any.
    #[must_use]
    pub fn room(&self) -> Option<&str> {
        self.room.as_deref()
    }

    /// The user root a remote device chains to.
    #[must_use]
    pub const fn user_public_key(&self) -> Option<&[u8; 32]> {
        self.user_public_key.as_ref()
    }

    /// Whether the caller acts from a foreign device.
    #[must_use]
    pub const fn is_foreign_device(&self) -> bool {
        self.foreign
    }

    /// The remote device that sent the request.
    #[must_use]
    pub const fn device_public_key(&self) -> Option<&[u8; 32]> {
        self.device_public_key.as_ref()
    }
}

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Which published action an invocation names on the host.
///
/// `provider` is an opaque token the host issued in a [`CataloguePage`]
/// (which live app published the action); `address` is the app's own stable
/// resource/action pair. A caller copies both from a catalogue entry and
/// never constructs a provider itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteActionTarget {
    pub provider: String,
    pub address: ActionAddress,
}

/// What an invocation carries for the action's declared input.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum InvokeArgument {
    /// The action takes nothing.
    #[default]
    None,
    /// The single text input a prompted action collects.
    Text(String),
    /// The typed argument object checked against the action's parameters.
    Json(serde_json::Map<String, serde_json::Value>),
}

impl InvokeArgument {
    /// The single-slot form the local invocation path accepts: text as is,
    /// a typed object behind [`ARGUMENTS_JSON_PREFIX`].
    #[must_use]
    pub fn to_slot(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Text(text) => Some(text.clone()),
            Self::Json(object) => Some(format!(
                "{ARGUMENTS_JSON_PREFIX}{}",
                serde_json::Value::Object(object.clone())
            )),
        }
    }
}

/// A request to run one published action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvokeRequest {
    /// Chosen by the caller and reused verbatim on retry; the host runs the
    /// action at most once per id.
    pub invocation: InvocationId,
    pub target: RemoteActionTarget,
    #[serde(default)]
    pub argument: InvokeArgument,
    /// Human provenance carried into history. It never grants authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ActionContext>,
    /// The caller's explicit confirmation, for an action whose contract
    /// requires one. The host checks it against the contract; it is
    /// evidence, not authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Confirmation>,
    /// The catalogue revision the caller chose the target from, for a
    /// provider whose actions are resolved live at that revision. A host
    /// refuses a target that no longer exists at it rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The live invocation this request continues: an edit an effect
    /// submits while running for someone else. The host admits it as that
    /// invocation's original caller, with their grants, never as the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<InvocationId>,
}

/// Explicit confirmation carried with an invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confirmation {
    /// The caller confirmed the action.
    Confirmed,
    /// The caller typed the phrase the contract asks for.
    Phrase(String),
}

/// Provider token naming the calling application's own connection, for an
/// app that routes one of its own actions through the host.
pub const OWN_PROVIDER: &str = "self";

/// Prefix of the provider token for actions a workspace itself offers
/// rather than an application; see [`workspace_provider`].
pub const WORKSPACE_PROVIDER: &str = "workspace";

/// Resource prefix of a workspace's "open this application" actions.
pub const APPLICATION_RESOURCE_PREFIX: &str = "app.";

/// Action id that opens an application into a workspace.
pub const APPLICATION_OPEN_ACTION: &str = "open";

/// Action id that closes a running application's windows in a workspace.
pub const APPLICATION_CLOSE_ACTION: &str = "close";

/// The provider token for the actions workspace `room` itself offers.
#[must_use]
pub fn workspace_provider(room: &str) -> String {
    format!("{WORKSPACE_PROVIDER}:{room}")
}

/// The room a workspace provider token names, or `None` for an application
/// provider.
#[must_use]
pub fn workspace_provider_room(provider: &str) -> Option<&str> {
    provider
        .strip_prefix(WORKSPACE_PROVIDER)
        .and_then(|rest| rest.strip_prefix(':'))
}

/// The target that opens `app_id` into workspace `room`. An invocation may
/// carry the content hash of bytes to open read-only as its text argument.
#[must_use]
pub fn application_open_target(room: &str, app_id: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(room),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{app_id}"),
            APPLICATION_OPEN_ACTION,
        ),
    }
}

/// Opening another installed app into the workspace, optionally with
/// verified content it opens read-only: Photos with a captured picture, a
/// player with a recorded clip. An app offers it as an action on its own
/// resources ([`ApplicationLaunch::spec`]); the host routes the admitted
/// invocation through the workspace broker, which stages the bytes and owns
/// the launched process. The source app never hands over a local path.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplicationLaunch {
    /// The installed app to open, by its id (`so.tilo.photos`).
    pub app_id: String,
    /// The content address hash of what it opens; the app opens empty
    /// without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl ApplicationLaunch {
    /// The operation type the host routes to the broker.
    pub const TYPE: &'static str = "tilo.application.launch";

    /// Open `app_id` empty.
    #[must_use]
    pub fn app(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: None,
        }
    }

    /// Open `app_id` with the content whose hash is `content_hash`.
    #[must_use]
    pub fn with_content(app_id: impl Into<String>, content_hash: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            content_hash: Some(content_hash.into()),
        }
    }

    /// An app id of letters, digits, dots and dashes, and a content hash
    /// (when given) of 64 lowercase hex digits.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.app_id.is_empty()
            && self.app_id.len() <= 128
            && self
                .app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            && self.content_hash.as_ref().is_none_or(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
    }

    /// The argument the broker's application `open` takes: the content
    /// hash as its text, or nothing.
    #[must_use]
    pub fn argument(&self) -> InvokeArgument {
        self.content_hash
            .clone()
            .map_or(InvokeArgument::None, InvokeArgument::Text)
    }

    /// Opening `app_id` with content the caller names as the action's
    /// `content_hash` argument: a control that hands over whatever it
    /// shows now.
    #[must_use]
    pub fn content_spec(
        app_id: &str,
        id: &str,
        label: &str,
        description: &str,
    ) -> crate::ActionSpec {
        let app_id = app_id.to_owned();
        crate::ActionSpec::typed_operation(
            id,
            label,
            description,
            crate::OperationKind::Effect,
            Self::TYPE,
            vec![crate::ActionParam::required(
                "content_hash",
                crate::ParamType::Text,
                "Content address hash of what the app opens",
            )],
            move |args| {
                let launch = Self::with_content(
                    app_id.clone(),
                    args.text("content_hash").unwrap_or_default(),
                );
                crate::EncodedAction::encode_operation(Self::TYPE, &launch).unwrap_or_else(|_| {
                    crate::EncodedAction {
                        type_id: Self::TYPE.to_owned(),
                        payload: Vec::new(),
                    }
                })
            },
        )
    }

    /// This launch as the action `id` an app offers on a resource of its
    /// own, taking no arguments.
    #[must_use]
    pub fn spec(&self, id: &str, label: &str, description: &str) -> crate::ActionSpec {
        let encoded =
            crate::EncodedAction::encode_operation(Self::TYPE, self).unwrap_or_else(|_| {
                crate::EncodedAction {
                    type_id: Self::TYPE.to_owned(),
                    payload: Vec::new(),
                }
            });
        crate::ActionSpec::typed_operation(
            id,
            label,
            description,
            crate::OperationKind::Effect,
            Self::TYPE,
            Vec::new(),
            move |_| encoded.clone(),
        )
    }
}

/// Prefix of the provider token naming an application in a workspace
/// whether or not it is running; see [`application_action_target`].
pub const APPLICATION_PROVIDER: &str = "application";

/// The target of the action `app_id` offers on `resource` in workspace
/// `room`. The host delivers it to the running app, or launches the app into
/// the workspace first and delivers it once the app offers the action; the
/// app's own published contract admits it either way.
#[must_use]
pub fn application_action_target(
    room: &str,
    app_id: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: format!("{APPLICATION_PROVIDER}:{room}:{app_id}"),
        address: ActionAddress::new(resource, action),
    }
}

/// The room and application an [`application_action_target`] provider
/// token names, or `None` for any other provider.
#[must_use]
pub fn application_provider(provider: &str) -> Option<(&str, &str)> {
    let (room, app_id) = provider
        .strip_prefix(APPLICATION_PROVIDER)?
        .strip_prefix(':')?
        .split_once(':')?;
    (!room.is_empty() && !app_id.is_empty() && !app_id.contains(':')).then_some((room, app_id))
}

/// Where an invocation stands, as reported to its caller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReceiptStatus {
    /// Admitted and running; follow it with `Observe`.
    Accepted,
    /// Finished and produced `output`.
    Completed { output: ActionOutput },
    /// Ran and failed.
    Failed { error: ActionFailure },
    /// Never admitted: nothing ran.
    Rejected { error: ActionHostError },
    /// Stopped at the caller's request.
    Cancelled { message: Option<String> },
}

impl ReceiptStatus {
    /// Whether the invocation can no longer change.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Accepted)
    }
}

/// The host's answer to an [`InvokeRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationReceipt {
    pub invocation: InvocationId,
    pub status: ReceiptStatus,
}

/// The latest lifecycle update of one invocation, carrying the caller's
/// [`InvocationId`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationState {
    pub update: InvocationUpdate,
}

impl InvocationState {
    /// Wrap a host-side update, rebinding it to the caller's invocation id.
    #[must_use]
    pub fn new(invocation: InvocationId, update: InvocationUpdate) -> Self {
        Self {
            update: update.with_invocation(invocation),
        }
    }

    #[must_use]
    pub fn invocation(&self) -> InvocationId {
        self.update.invocation()
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.update.is_terminal()
    }

    /// The receipt this state amounts to: accepted while running, the
    /// outcome once terminal.
    #[must_use]
    pub fn receipt(&self) -> InvocationReceipt {
        let status = match &self.update {
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => ReceiptStatus::Accepted,
            InvocationUpdate::Succeeded { output, .. } => ReceiptStatus::Completed {
                output: output.clone(),
            },
            InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed {
                error: error.clone(),
            },
            InvocationUpdate::Cancelled { message, .. } => ReceiptStatus::Cancelled {
                message: message.clone(),
            },
        };
        InvocationReceipt {
            invocation: self.invocation(),
            status,
        }
    }
}

/// Why a host would not do what was asked. Every variant means nothing ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ActionHostError {
    /// The host serves no actions right now (no live apps, service off).
    #[error("remote actions are unavailable: {reason}")]
    Unavailable { reason: String },
    /// The target is not offered to this caller — unknown and forbidden are
    /// deliberately the same answer.
    #[error("no such action is offered")]
    UnknownAction,
    /// The caller's authority does not cover the action.
    #[error("the caller may not invoke this action")]
    NotPermitted,
    /// The action requires a confirmation step this request did not carry.
    #[error("the action requires confirmation")]
    ConfirmationRequired,
    /// The request is malformed for this action (arguments, input).
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },
    /// The invocation id is unknown to this caller.
    #[error("no such invocation")]
    UnknownInvocation,
    /// The invocation id was already used for a different request.
    #[error("the invocation id was already used for a different request")]
    InvocationConflict,
    /// The invocation cannot be cancelled (finished, or never cancellable).
    #[error("the invocation cannot be cancelled: {reason}")]
    NotCancellable { reason: String },
    /// The host refused for a reason of its own.
    #[error("refused: {reason}")]
    Refused { reason: String },
}

impl ActionHostError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "remote_actions_unavailable",
            Self::UnknownAction => "unknown_action",
            Self::NotPermitted => "not_permitted",
            Self::ConfirmationRequired => "action_confirmation_required",
            Self::InvalidRequest { .. } => "invalid_arguments",
            Self::UnknownInvocation => "unknown_invocation",
            Self::InvocationConflict => "invocation_conflict",
            Self::NotCancellable { .. } => "not_cancellable",
            Self::Refused { .. } => "refused",
        }
    }
}

/// A peer that runs actions on behalf of authenticated remote callers.
///
/// Implementations resolve authority from `caller` alone and must be
/// idempotent per [`InvocationId`]: invoking an id again with the same
/// request reports the original invocation, with a different request fails
/// with [`ActionHostError::InvocationConflict`]. [`InvocationLedger`]
/// implements that rule.
pub trait ActionHost {
    /// End this admitted transport run without cancelling its app jobs.
    fn retire_execution(&self, caller: &Caller) {
        caller.retire_execution();
    }

    /// One page of what `caller` is offered. An action the caller may not
    /// invoke is absent, not flagged.
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>>;

    /// Admit and start one invocation, or report the one already admitted
    /// under the same id. Never blocks: the answer is the admission (an
    /// accepted receipt, an immediate outcome, or a refusal) once the host
    /// has it; completion is followed with [`Self::observe`].
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>>;

    /// The latest state of an invocation `caller` started, or `None` for an
    /// id this caller does not own.
    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState>;

    /// Resolves the next time invocation `id` (owned by `caller`) may have
    /// changed state, so a waiter observes it again then instead of on a
    /// timer. `None` when this host cannot tell; the caller then observes on
    /// its own schedule.
    ///
    /// Wakeups may be spurious: the waiter must [`observe`](Self::observe)
    /// again and tolerate finding nothing new. A host that answers `Some`
    /// must eventually resolve it even when nothing changes, so a change it
    /// failed to signal costs latency, never a stalled waiter.
    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let _ = (caller, id);
        None
    }

    /// Ask a running invocation to stop.
    ///
    /// # Errors
    ///
    /// Returns [`ActionHostError::UnknownInvocation`] for an id this caller
    /// does not own, or [`ActionHostError::NotCancellable`].
    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError>;
}

/// Everything that crosses the wire for remote action use.
#[allow(
    clippy::large_enum_variant,
    reason = "a message is built, encoded once and dropped; boxing would only add noise to every constructor and match"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RemoteActionMessage {
    /// Ask for one page of the actions offered to the caller.
    CatalogueRequest {
        correlation: u64,
        #[serde(default)]
        query: CatalogueQuery,
    },
    /// Answer to a [`Self::CatalogueRequest`] with the same correlation.
    CatalogueResponse {
        correlation: u64,
        result: Result<CataloguePage, ActionHostError>,
    },
    /// Run one action.
    Invoke(InvokeRequest),
    /// Answer to [`Self::Invoke`]; a refusal is [`ReceiptStatus::Rejected`].
    Receipt(InvocationReceipt),
    /// Ask for an invocation's latest state.
    Observe { invocation: InvocationId },
    /// Answer to [`Self::Observe`]; `None` for an id the caller does not own.
    /// A host may also push this unprompted as an invocation progresses.
    Update {
        invocation: InvocationId,
        state: Option<InvocationState>,
    },
    /// Ask a running invocation to stop.
    Cancel { invocation: InvocationId },
    /// Answer to [`Self::Cancel`].
    CancelResponse {
        invocation: InvocationId,
        result: Result<(), ActionHostError>,
    },
    /// Ask a device for a room record it holds, from `query.offset` on.
    /// The same authenticated device-to-device route as an action, but
    /// answered by the device from what its app last made durable, not by
    /// an action host: a record is readable where it is kept whether or not
    /// its app is running.
    RecordRequest {
        correlation: u64,
        query: RecordQuery,
    },
    /// Answer to [`Self::RecordRequest`] with the same correlation: the next
    /// piece of the record, or why it is not given.
    RecordResponse {
        correlation: u64,
        result: Result<RecordPiece, ActionHostError>,
    },
}

/// Largest piece of a record one answer carries, before encoding.
pub const MAX_RECORD_PIECE_BYTES: usize = 384 * 1024;

/// Largest record a device fetches from another at all.
pub const MAX_FETCHED_RECORD_BYTES: u64 = 64 * 1024 * 1024;

/// Which record to read, and from where in its stored bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordQuery {
    /// The room the record belongs to.
    pub room: String,
    /// The application whose record it is.
    pub app: String,
    /// The record's id in that room.
    pub record: String,
    /// Where in the stored document this piece starts.
    pub offset: u64,
}

/// One piece of a stored record document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordPiece {
    /// The whole document's length.
    pub total: u64,
    /// Where this piece starts.
    pub offset: u64,
    /// The piece, hex-encoded so the frame stays self-describing text.
    bytes: String,
}

impl RecordPiece {
    #[must_use]
    pub fn new(total: u64, offset: u64, bytes: &[u8]) -> Self {
        Self {
            total,
            offset,
            bytes: hex(bytes),
        }
    }

    /// The piece's bytes; `None` when they are not well-formed.
    #[must_use]
    pub fn bytes(&self) -> Option<Vec<u8>> {
        if !self.bytes.len().is_multiple_of(2) || self.bytes.len() > MAX_RECORD_PIECE_BYTES * 2 {
            return None;
        }
        (0..self.bytes.len())
            .step_by(2)
            .map(|at| {
                self.bytes
                    .get(at..at.saturating_add(2))
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            })
            .collect()
    }
}

impl RemoteActionMessage {
    /// Whether a host answers this message (as opposed to a caller
    /// receiving it).
    #[must_use]
    pub const fn is_request(&self) -> bool {
        matches!(
            self,
            Self::CatalogueRequest { .. }
                | Self::Invoke(_)
                | Self::Observe { .. }
                | Self::Cancel { .. }
                | Self::RecordRequest { .. }
        )
    }
}

/// A versioned [`RemoteActionMessage`], the unit a transport carries.
///
/// The encoding is self-describing JSON: action arguments and outputs are
/// open JSON values, and a relay forwarding the bytes need not understand
/// them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteActionFrame {
    pub version: u16,
    pub message: RemoteActionMessage,
}

/// Why bytes are not a usable [`RemoteActionFrame`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoteFrameError {
    #[error("remote action frame of {len} bytes exceeds the limit")]
    TooLarge { len: usize },
    #[error("remote action frame version {version} is not supported")]
    UnsupportedVersion { version: u16 },
    #[error("malformed remote action frame: {0}")]
    Malformed(String),
}

impl RemoteActionFrame {
    /// Wrap `message` at this build's [`REMOTE_ACTIONS_VERSION`].
    #[must_use]
    pub const fn new(message: RemoteActionMessage) -> Self {
        Self {
            version: REMOTE_ACTIONS_VERSION,
            message,
        }
    }

    /// Encode for a transport.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteFrameError::TooLarge`] past
    /// [`MAX_REMOTE_ACTION_FRAME_BYTES`], or `Malformed` if a value cannot
    /// be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, RemoteFrameError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))?;
        if bytes.len() > MAX_REMOTE_ACTION_FRAME_BYTES {
            return Err(RemoteFrameError::TooLarge { len: bytes.len() });
        }
        Ok(bytes)
    }

    /// Decode bytes from a transport. The version is checked before the
    /// message, so a frame from a newer peer is refused by version rather
    /// than misread.
    ///
    /// # Errors
    ///
    /// Returns a [`RemoteFrameError`] for oversized, unknown-version or
    /// malformed input.
    pub fn decode(bytes: &[u8]) -> Result<Self, RemoteFrameError> {
        #[derive(Deserialize)]
        struct Version {
            version: u16,
        }
        if bytes.len() > MAX_REMOTE_ACTION_FRAME_BYTES {
            return Err(RemoteFrameError::TooLarge { len: bytes.len() });
        }
        let Version { version } = serde_json::from_slice(bytes)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))?;
        if version != REMOTE_ACTIONS_VERSION {
            return Err(RemoteFrameError::UnsupportedVersion { version });
        }
        serde_json::from_slice(bytes)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))
    }
}

/// Answer one request from `caller` against `host`.
///
/// Returns `None` for a message that is not a request (a response arriving
/// at a host is dropped, never answered). A refused invocation becomes a
/// [`ReceiptStatus::Rejected`] receipt so the caller always learns the fate
/// of the id it chose; an answer the host abandons becomes a refusal too.
pub fn serve<H: ActionHost + ?Sized>(
    host: &H,
    caller: &Caller,
    message: RemoteActionMessage,
) -> Option<Pending<RemoteActionMessage>> {
    let abandoned = refuse(
        &message,
        ActionHostError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    )?;
    let answer = match message {
        RemoteActionMessage::CatalogueRequest { correlation, query } => host
            .query(caller, query)
            .map(move |result| RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            }),
        RemoteActionMessage::Invoke(request) => {
            let invocation = request.invocation;
            host.invoke(caller, request).map(move |result| {
                RemoteActionMessage::Receipt(result.unwrap_or_else(|error| InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                }))
            })
        }
        RemoteActionMessage::Observe { invocation } => {
            Pending::ready(RemoteActionMessage::Update {
                invocation,
                state: host.observe(caller, &invocation),
            })
        }
        RemoteActionMessage::Cancel { invocation } => {
            Pending::ready(RemoteActionMessage::CancelResponse {
                invocation,
                result: host.cancel(caller, &invocation),
            })
        }
        // A record is read by the device that keeps it, never through an
        // action host; a host asked for one says so.
        RemoteActionMessage::RecordRequest { correlation, .. } => {
            Pending::ready(RemoteActionMessage::RecordResponse {
                correlation,
                result: Err(ActionHostError::Unavailable {
                    reason: "records are read from the device that keeps them".into(),
                }),
            })
        }
        RemoteActionMessage::CatalogueResponse { .. }
        | RemoteActionMessage::Receipt(_)
        | RemoteActionMessage::Update { .. }
        | RemoteActionMessage::CancelResponse { .. }
        | RemoteActionMessage::RecordResponse { .. } => return None,
    };
    // An abandoned answer still answers: the caller hears a refusal.
    let (completer, answered) = pending();
    answer.then(move |answer| completer.complete(answer.unwrap_or(abandoned)));
    Some(answered)
}

/// The answer that refuses request `message` with `error` — for a host that
/// cannot be reached or is not running. Returns `None` for a message that
/// is not a request.
#[must_use]
pub fn refuse(
    message: &RemoteActionMessage,
    error: ActionHostError,
) -> Option<RemoteActionMessage> {
    Some(match message {
        RemoteActionMessage::CatalogueRequest { correlation, .. } => {
            RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::Invoke(request) => RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Rejected { error },
        }),
        RemoteActionMessage::Observe { invocation } => RemoteActionMessage::Update {
            invocation: *invocation,
            state: None,
        },
        RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
            invocation: *invocation,
            result: Err(error),
        },
        RemoteActionMessage::RecordRequest { correlation, .. } => {
            RemoteActionMessage::RecordResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::CatalogueResponse { .. }
        | RemoteActionMessage::Receipt(_)
        | RemoteActionMessage::Update { .. }
        | RemoteActionMessage::CancelResponse { .. }
        | RemoteActionMessage::RecordResponse { .. } => return None,
    })
}

struct LedgerEntry<T> {
    caller: Caller,
    request: InvokeRequest,
    /// `None` while a start begun with [`InvocationLedger::reserve`] is in
    /// flight.
    handle: Option<T>,
}

/// The idempotency ledger behind [`ActionHost::invoke`].
///
/// Maps each admitted [`InvocationId`] to the caller and request that
/// admitted it and to the host's own handle for the running job. A retry
/// with the same caller and request returns the existing handle without
/// starting anything; any other reuse of the id is a conflict. Only admitted
/// invocations are remembered — a refused one ran nothing, so retrying it
/// may be admitted later. The oldest entries are forgotten past `capacity`.
pub struct InvocationLedger<T> {
    entries: HashMap<InvocationId, LedgerEntry<T>>,
    order: VecDeque<InvocationId>,
    capacity: usize,
}

impl<T> Default for InvocationLedger<T> {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LEDGER_CAPACITY)
    }
}

/// What [`InvocationLedger::reserve`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The id is newly claimed; the caller must now start the job.
    Reserved,
    /// The same caller and request were already admitted.
    Admitted,
}

/// Whether [`InvocationLedger::admit`] started a job or found one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Started,
    Retried,
}

impl<T> InvocationLedger<T> {
    /// A ledger remembering at most `capacity` invocations (at least one).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Admit `request` for `caller`, calling `start` only if the id is new.
    ///
    /// # Errors
    ///
    /// Returns [`ActionHostError::InvocationConflict`] when the id was
    /// admitted for another caller or request, or whatever `start` returns.
    pub fn admit(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
        start: impl FnOnce() -> Result<T, ActionHostError>,
    ) -> Result<(&T, Admission), ActionHostError> {
        let id = request.invocation;
        let admission = match self.reserve(caller, request)? {
            Reservation::Admitted => Admission::Retried,
            Reservation::Reserved => match start() {
                Ok(handle) => {
                    self.complete(&id, handle);
                    Admission::Started
                }
                Err(error) => {
                    self.abandon(&id);
                    return Err(error);
                }
            },
        };
        self.entries
            .get(&id)
            .and_then(|entry| entry.handle.as_ref())
            .map(|handle| (handle, admission))
            .ok_or(ActionHostError::UnknownInvocation)
    }

    /// First half of an admission whose start completes later (for example
    /// across an asynchronous resolution): claim the id for `caller` and
    /// `request`, then [`complete`](Self::complete) or
    /// [`abandon`](Self::abandon) it.
    ///
    /// # Errors
    ///
    /// Returns [`ActionHostError::InvocationConflict`] when the id belongs to
    /// another caller or request, and [`ActionHostError::Unavailable`] while
    /// an earlier start of the same invocation is still in flight.
    pub fn reserve(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Reservation, ActionHostError> {
        let id = request.invocation;
        if let Some(entry) = self.entries.get(&id) {
            if entry.caller != *caller || entry.request != *request {
                return Err(ActionHostError::InvocationConflict);
            }
            return if entry.handle.is_some() {
                Ok(Reservation::Admitted)
            } else {
                Err(ActionHostError::Unavailable {
                    reason: "the invocation is still being admitted; retry".into(),
                })
            };
        }
        while self.order.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(id);
        self.entries.insert(
            id,
            LedgerEntry {
                caller: caller.clone(),
                request: request.clone(),
                handle: None,
            },
        );
        Ok(Reservation::Reserved)
    }

    /// Record the host's handle for a reserved invocation.
    pub fn complete(&mut self, id: &InvocationId, handle: T) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.handle = Some(handle);
        }
    }

    /// Release a reservation whose start failed: nothing ran, so the id may
    /// be admitted later.
    pub fn abandon(&mut self, id: &InvocationId) {
        if self
            .entries
            .get(id)
            .is_some_and(|entry| entry.handle.is_none())
        {
            self.entries.remove(id);
            self.order.retain(|candidate| candidate != id);
        }
    }

    /// The handle for an invocation `caller` admitted.
    #[must_use]
    pub fn get(&self, caller: &Caller, id: &InvocationId) -> Option<&T> {
        self.entries
            .get(id)
            .filter(|entry| entry.caller == *caller)
            .and_then(|entry| entry.handle.as_ref())
    }

    /// Mutable access to the handle for an invocation `caller` admitted.
    pub fn get_mut(&mut self, caller: &Caller, id: &InvocationId) -> Option<&mut T> {
        self.entries
            .get_mut(id)
            .filter(|entry| entry.caller == *caller)
            .and_then(|entry| entry.handle.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::{ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind};

    #[test]
    fn execution_lifetime_is_shared_without_changing_caller_identity() {
        let first = Caller::local_person("person:owner").in_new_execution();
        let retained = first.clone();
        let second = first.clone().in_new_execution();
        let hash = |caller: &Caller| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(caller, &mut hasher);
            std::hash::Hasher::finish(&hasher)
        };
        let before = hash(&first);
        first.retire_execution();
        assert!(!retained.execution_is_live());
        assert!(second.execution_is_live());
        assert_eq!(first, retained);
        assert_eq!(hash(&first), before);
        let reconnected = retained.in_new_execution();
        assert!(reconnected.execution_is_live());
        assert_ne!(reconnected.execution_id(), first.execution_id());
    }

    fn alice() -> Caller {
        Caller::authenticated([1; 32], [2; 32])
    }

    fn mallory() -> Caller {
        Caller::authenticated([9; 32], [8; 32])
    }

    #[test]
    fn an_application_action_target_names_its_room_and_app() {
        let target = application_action_target("room-1", "so.tilo.chat", "chat.workspace", "call");
        assert_eq!(
            application_provider(&target.provider),
            Some(("room-1", "so.tilo.chat"))
        );
        assert_eq!(target.address, ActionAddress::new("chat.workspace", "call"));
        for other in [
            "7",
            "workspace:room-1",
            "application:room-1",
            "application::so.tilo.chat",
            "application:room-1:",
            "application:room-1:a:b",
        ] {
            assert_eq!(application_provider(other), None, "{other}");
        }
    }

    fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
        ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: "notes.note".into(),
            resource_label: "Groceries".into(),
            action_label: "Archive".into(),
            description: Some("Move the note to the archive".into()),
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract: ActionContract::default(),
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }

    /// A host with one action whose runs are counted, so a test can prove
    /// how many times the action actually started.
    struct CountingHost {
        runs: Cell<u32>,
        ledger: RefCell<InvocationLedger<RefCell<InvocationUpdate>>>,
    }

    impl CountingHost {
        fn new() -> Self {
            Self {
                runs: Cell::new(0),
                ledger: RefCell::new(InvocationLedger::default()),
            }
        }

        fn finish(&self, caller: &Caller, id: &InvocationId) {
            if let Some(update) = self.ledger.borrow().get(caller, id) {
                *update.borrow_mut() = InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                };
            }
        }
    }

    impl ActionHost for CountingHost {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            Pending::ready(Ok(CataloguePage::from_rows(
                "1".into(),
                vec![catalogue_row("note:1", "archive")],
                &query,
            )))
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            Pending::ready((|| {
                if request.target.address != ActionAddress::new("note:1", "archive") {
                    return Err(ActionHostError::UnknownAction);
                }
                let mut ledger = self.ledger.borrow_mut();
                let (update, _) = ledger.admit(caller, &request, || {
                    self.runs.set(self.runs.get().saturating_add(1));
                    Ok(RefCell::new(InvocationUpdate::Accepted {
                        invocation: request.invocation,
                    }))
                })?;
                Ok(InvocationState::new(request.invocation, update.borrow().clone()).receipt())
            })())
        }

        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.ledger
                .borrow()
                .get(caller, id)
                .map(|update| InvocationState::new(*id, update.borrow().clone()))
        }

        fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
            let ledger = self.ledger.borrow();
            let update = ledger
                .get(caller, id)
                .ok_or(ActionHostError::UnknownInvocation)?;
            if update.borrow().is_terminal() {
                return Err(ActionHostError::NotCancellable {
                    reason: "finished".into(),
                });
            }
            *update.borrow_mut() = InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            };
            Ok(())
        }
    }

    fn catalogue_row(resource: &str, action: &str) -> CatalogueRow {
        CatalogueRow {
            provider: "7".into(),
            app_id: "notes".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.into(),
                kind: "notes.note".into(),
                label: "Groceries".into(),
                presentation: None,
            },
            action: Some(descriptor(resource, action)),
            has_children: false,
            visibility: None,
        }
    }

    /// The answer of a host that answers immediately.
    fn now<T>(mut answer: Pending<T>) -> T {
        match answer.poll_ready() {
            PendingPoll::Ready(value) => value,
            PendingPoll::Waiting | PendingPoll::Abandoned => panic!("the host answers at once"),
        }
    }

    fn archive(invocation: InvocationId) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: "7".into(),
                address: ActionAddress::new("note:1", "archive"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
        }
    }

    fn round_trip(message: RemoteActionMessage) -> RemoteActionMessage {
        let bytes = RemoteActionFrame::new(message).encode().unwrap();
        RemoteActionFrame::decode(&bytes).unwrap().message
    }

    /// One of each catalogue and invoke message shape.
    fn request_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("title".into(), serde_json::json!("Weekly"));
        vec![
            RemoteActionMessage::CatalogueRequest {
                correlation: 3,
                query: CatalogueQuery::default(),
            },
            RemoteActionMessage::CatalogueRequest {
                correlation: 5,
                query: CatalogueQuery {
                    provider: Some("7".into()),
                    parent_resource: Some("folder:a".into()),
                    needle: "gro".into(),
                    cursor: vec![1, 2],
                    revision: Some("r1".into()),
                    max_results: 8,
                    limits: Some(CatalogueLimits {
                        max_bytes: Some(4096),
                        max_work: None,
                        max_depth: Some(1),
                    }),
                    ..CatalogueQuery::default()
                },
            },
            RemoteActionMessage::CatalogueResponse {
                correlation: 3,
                result: now(CountingHost::new().query(&alice(), CatalogueQuery::default())),
            },
            RemoteActionMessage::CatalogueResponse {
                correlation: 4,
                result: Err(ActionHostError::Unavailable {
                    reason: "no apps".into(),
                }),
            },
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                argument: InvokeArgument::Json(arguments),
                context: Some(ActionContext::for_resource(
                    "Archive",
                    "note:1",
                    "notes.note",
                    "Groceries",
                )),
                ..archive(invocation)
            }),
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                argument: InvokeArgument::Text("hello".into()),
                ..archive(invocation)
            }),
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                confirmation: Some(Confirmation::Phrase("delete it".into())),
                ..archive(invocation)
            }),
        ]
    }

    /// One of each receipt, update and cancel message shape.
    fn outcome_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
        vec![
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(serde_json::json!({"archived": true})),
                        resources: vec!["notes.note:1".into()],
                    },
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Failed {
                    error: ActionFailure {
                        code: "disk_full".into(),
                        message: "no space".into(),
                        details: None,
                        retryable: true,
                    },
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::InvocationConflict,
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Cancelled {
                    message: Some("stopped".into()),
                },
            }),
            RemoteActionMessage::Observe { invocation },
            RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Progress {
                        invocation,
                        sequence: 2,
                        phase: "copying".into(),
                        completed: Some(1),
                        total: Some(4),
                        message: None,
                    },
                )),
            },
            RemoteActionMessage::Update {
                invocation,
                state: None,
            },
            RemoteActionMessage::Cancel { invocation },
            RemoteActionMessage::CancelResponse {
                invocation,
                result: Err(ActionHostError::NotCancellable {
                    reason: "finished".into(),
                }),
            },
        ]
    }

    #[test]
    fn a_record_piece_round_trips_and_action_hosts_never_serve_records() {
        let bytes: Vec<u8> = (0..=255).collect();
        let request = RemoteActionMessage::RecordRequest {
            correlation: 4,
            query: RecordQuery {
                room: "room".into(),
                app: "dev.tilo.notes".into(),
                record: "uuid:1".into(),
                offset: 0,
            },
        };
        assert!(request.is_request());
        let answer = RemoteActionMessage::RecordResponse {
            correlation: 4,
            result: Ok(RecordPiece::new(256, 0, &bytes)),
        };
        let wire = RemoteActionFrame::new(answer.clone()).encode().unwrap();
        let decoded = RemoteActionFrame::decode(&wire).unwrap().message;
        assert_eq!(decoded, answer);
        let RemoteActionMessage::RecordResponse {
            result: Ok(piece), ..
        } = decoded
        else {
            panic!("a record piece")
        };
        assert_eq!(piece.bytes(), Some(bytes));
        assert!(matches!(
            refuse(&request, ActionHostError::NotPermitted),
            Some(RemoteActionMessage::RecordResponse {
                correlation: 4,
                result: Err(ActionHostError::NotPermitted)
            })
        ));
    }

    /// Every message round-trips through a frame, frames from another version are refused not
    /// misread, and the wire names versions and messages explicitly.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_remote_wire_is_versioned_and_explicit() {
        every_message_round_trips_through_a_frame();
        frames_from_another_version_are_refused_not_misread();
        the_wire_names_versions_and_messages_explicitly();
    }

    fn every_message_round_trips_through_a_frame() {
        let invocation = InvocationId(uuid::Uuid::from_u128(7));
        let messages = request_messages(invocation)
            .into_iter()
            .chain(outcome_messages(invocation));
        for message in messages {
            assert_eq!(round_trip(message.clone()), message);
        }
    }

    fn frames_from_another_version_are_refused_not_misread() {
        let mut frame = serde_json::to_value(RemoteActionFrame::new(
            RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: CatalogueQuery::default(),
            },
        ))
        .unwrap();
        frame["version"] = serde_json::json!(REMOTE_ACTIONS_VERSION + 1);
        // A newer peer may also use a message this build has never seen.
        frame["message"] = serde_json::json!({"Subscribe": {"topic": 1}});
        let bytes = serde_json::to_vec(&frame).unwrap();
        assert_eq!(
            RemoteActionFrame::decode(&bytes),
            Err(RemoteFrameError::UnsupportedVersion {
                version: REMOTE_ACTIONS_VERSION + 1
            })
        );
        assert!(matches!(
            RemoteActionFrame::decode(b"{\"version\":1,\"message\":{\"Subscribe\":{}}}"),
            Err(RemoteFrameError::Malformed(_))
        ));
        assert!(matches!(
            RemoteActionFrame::decode(&vec![b' '; MAX_REMOTE_ACTION_FRAME_BYTES + 1]),
            Err(RemoteFrameError::TooLarge { .. })
        ));
    }

    fn the_wire_names_versions_and_messages_explicitly() {
        let bytes = RemoteActionFrame::new(RemoteActionMessage::Cancel {
            invocation: InvocationId(uuid::Uuid::nil()),
        })
        .encode()
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"version":1,"message":{"Cancel":{"invocation":"00000000-0000-0000-0000-000000000000"}}}"#
        );
    }

    /// A retried invocation reports the original and runs once; ids cannot be reused or borrowed;
    /// refusals are not remembered; the ledger forgets the oldest past capacity; a reservation holds
    /// the id until the start completes.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn remote_invocations_run_at_most_once_per_owned_id() {
        a_retried_invocation_reports_the_original_and_runs_once();
        an_invocation_id_cannot_be_reused_or_borrowed();
        a_refused_invocation_is_not_remembered();
        the_ledger_forgets_oldest_invocations_past_capacity();
        a_reservation_holds_the_id_until_the_start_completes();
    }

    #[test]
    fn admitted_executions_share_authority_but_cannot_borrow_invocations() {
        let first = alice().in_new_execution();
        let second = alice().in_new_execution();
        assert_eq!(first.principal(), second.principal());
        assert_eq!(first.room(), second.room());
        assert_ne!(first.execution_id(), second.execution_id());
        let host = CountingHost::new();
        let id = InvocationId::new();
        let admitted = now(host.invoke(&first, archive(id))).unwrap();
        assert_eq!(now(host.invoke(&first, archive(id))).unwrap(), admitted);
        assert_eq!(
            now(host.invoke(&second, archive(id))),
            Err(ActionHostError::InvocationConflict)
        );
        assert_eq!(host.observe(&second, &id), None);
        assert_eq!(
            host.cancel(&second, &id),
            Err(ActionHostError::UnknownInvocation)
        );
        host.finish(&first, &id);
        assert!(matches!(
            now(host.invoke(&first, archive(id))).unwrap().status,
            ReceiptStatus::Completed { .. }
        ));
        assert!(host.observe(&first, &id).is_some());
        assert!(host.observe(&second, &id).is_none());
        assert_eq!(host.runs.get(), 1);
    }

    fn a_retried_invocation_reports_the_original_and_runs_once() {
        let host = CountingHost::new();
        let id = InvocationId::new();
        let first = now(host.invoke(&alice(), archive(id))).unwrap();
        assert_eq!(first.status, ReceiptStatus::Accepted);
        let retry = now(host.invoke(&alice(), archive(id))).unwrap();
        assert_eq!(retry, first);
        assert_eq!(host.runs.get(), 1, "a retry never starts a second run");

        host.finish(&alice(), &id);
        let after = now(host.invoke(&alice(), archive(id))).unwrap();
        assert!(matches!(after.status, ReceiptStatus::Completed { .. }));
        assert_eq!(host.runs.get(), 1, "a retry after completion reports it");
    }

    fn an_invocation_id_cannot_be_reused_or_borrowed() {
        let host = CountingHost::new();
        let id = InvocationId::new();
        now(host.invoke(&alice(), archive(id))).unwrap();

        let mut different = archive(id);
        different.argument = InvokeArgument::Text("other".into());
        assert_eq!(
            now(host.invoke(&alice(), different)),
            Err(ActionHostError::InvocationConflict)
        );
        assert_eq!(
            now(host.invoke(&mallory(), archive(id))),
            Err(ActionHostError::InvocationConflict),
            "another principal cannot attach to someone else's invocation"
        );
        assert_eq!(host.observe(&mallory(), &id), None);
        assert_eq!(
            host.cancel(&mallory(), &id),
            Err(ActionHostError::UnknownInvocation)
        );
        assert_eq!(host.runs.get(), 1);
    }

    fn a_refused_invocation_is_not_remembered() {
        let mut ledger = InvocationLedger::<u32>::default();
        let request = archive(InvocationId::new());
        assert_eq!(
            ledger.admit(&alice(), &request, || Err(ActionHostError::NotPermitted)),
            Err(ActionHostError::NotPermitted)
        );
        let (handle, admission) = ledger.admit(&alice(), &request, || Ok(5)).unwrap();
        assert_eq!((*handle, admission), (5, Admission::Started));
    }

    fn the_ledger_forgets_oldest_invocations_past_capacity() {
        let mut ledger = InvocationLedger::<u32>::with_capacity(2);
        let requests: Vec<_> = (0..3).map(|_| archive(InvocationId::new())).collect();
        for (n, request) in (0u32..).zip(&requests) {
            ledger.admit(&alice(), request, || Ok(n)).unwrap();
        }
        assert_eq!(ledger.get(&alice(), &requests[0].invocation), None);
        assert_eq!(ledger.get(&alice(), &requests[2].invocation), Some(&2));
    }

    /// Serve answers requests and ignores responses, typed arguments use the local invocation slot,
    /// a remote device is the person its root names, and a refusal answers every request and nothing
    /// else.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn serving_answers_each_request_as_the_person_the_device_belongs_to() {
        serve_answers_requests_and_ignores_responses();
        typed_arguments_use_the_local_invocation_slot();
        a_remote_device_is_the_person_its_root_names();
        a_refusal_answers_every_request_and_nothing_else();
    }

    fn serve_answers_requests_and_ignores_responses() {
        let host = CountingHost::new();
        let id = InvocationId::new();
        let Some(RemoteActionMessage::Receipt(receipt)) =
            serve(&host, &alice(), RemoteActionMessage::Invoke(archive(id))).map(now)
        else {
            panic!("an invoke is answered with a receipt");
        };
        assert_eq!(receipt.status, ReceiptStatus::Accepted);

        let mut unknown = archive(InvocationId::new());
        unknown.target.address = ActionAddress::new("note:2", "delete");
        assert_eq!(
            serve(
                &host,
                &alice(),
                RemoteActionMessage::Invoke(unknown.clone())
            )
            .map(now),
            Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: unknown.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::UnknownAction
                },
            }))
        );
        assert_eq!(
            serve(
                &host,
                &alice(),
                RemoteActionMessage::Cancel { invocation: id }
            )
            .map(now),
            Some(RemoteActionMessage::CancelResponse {
                invocation: id,
                result: Ok(()),
            })
        );
        let Some(RemoteActionMessage::Update {
            state: Some(state), ..
        }) = serve(
            &host,
            &alice(),
            RemoteActionMessage::Observe { invocation: id },
        )
        .map(now)
        else {
            panic!("an observe is answered with the invocation state");
        };
        assert!(matches!(
            state.receipt().status,
            ReceiptStatus::Cancelled { .. }
        ));
        assert!(serve(&host, &alice(), RemoteActionMessage::Receipt(receipt)).is_none());
    }

    fn typed_arguments_use_the_local_invocation_slot() {
        let mut object = serde_json::Map::new();
        object.insert("n".into(), serde_json::json!(2));
        assert_eq!(InvokeArgument::None.to_slot(), None);
        assert_eq!(
            InvokeArgument::Text("hi".into()).to_slot().as_deref(),
            Some("hi")
        );
        assert_eq!(
            InvokeArgument::Json(object).to_slot(),
            Some(format!("{ARGUMENTS_JSON_PREFIX}{{\"n\":2}}"))
        );
    }

    fn a_remote_device_is_the_person_its_root_names() {
        let caller = Caller::authenticated([0xAB; 32], [1; 32]);
        assert_eq!(
            caller.principal(),
            &CallerPrincipal::Person("ab".repeat(32)),
            "every device of one person is the same principal"
        );
        assert_eq!(caller.room(), None);
        let agent = Caller::local_agent("helper").in_room("room-1");
        assert_eq!(agent.principal(), &CallerPrincipal::Agent("helper".into()));
        assert_eq!(agent.room(), Some("room-1"));
        assert_eq!(agent.device_public_key(), None);
    }

    fn a_refusal_answers_every_request_and_nothing_else() {
        let error = ActionHostError::Unavailable {
            reason: "down".into(),
        };
        let id = InvocationId::new();
        assert_eq!(
            refuse(&invoke_message(id), error.clone()),
            Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: id,
                status: ReceiptStatus::Rejected {
                    error: error.clone()
                },
            }))
        );
        assert_eq!(
            refuse(
                &RemoteActionMessage::Observe { invocation: id },
                error.clone()
            ),
            Some(RemoteActionMessage::Update {
                invocation: id,
                state: None
            })
        );
        assert_eq!(
            refuse(
                &RemoteActionMessage::Update {
                    invocation: id,
                    state: None
                },
                error
            ),
            None
        );
    }

    fn invoke_message(id: InvocationId) -> RemoteActionMessage {
        RemoteActionMessage::Invoke(archive(id))
    }

    fn a_reservation_holds_the_id_until_the_start_completes() {
        let mut ledger = InvocationLedger::<u32>::default();
        let request = archive(InvocationId::new());
        assert_eq!(
            ledger.reserve(&alice(), &request),
            Ok(Reservation::Reserved)
        );
        assert!(
            matches!(
                ledger.reserve(&alice(), &request),
                Err(ActionHostError::Unavailable { .. })
            ),
            "a retry during the start neither runs twice nor pretends to be admitted"
        );
        assert_eq!(ledger.get(&alice(), &request.invocation), None);
        ledger.complete(&request.invocation, 9);
        assert_eq!(
            ledger.reserve(&alice(), &request),
            Ok(Reservation::Admitted)
        );
        assert_eq!(ledger.get(&alice(), &request.invocation), Some(&9));

        let failed = archive(InvocationId::new());
        assert_eq!(ledger.reserve(&alice(), &failed), Ok(Reservation::Reserved));
        ledger.abandon(&failed.invocation);
        assert_eq!(
            ledger.reserve(&alice(), &failed),
            Ok(Reservation::Reserved),
            "an abandoned start ran nothing and may be admitted again"
        );
    }
}
