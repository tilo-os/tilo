//! Page actions on any record of a workspace, answered by the host.
//!
//! An agent answers `actionsV1` only for the records it has open. A person's
//! page wants the whole workspace: every record's actions, whether or not anyone
//! has it open, the way the old broker offered them. [`WorkspaceActions`] is that
//! service on the host. It serves the `tilo_actions::remote` vocabulary
//! addressed to the host's own device:
//!
//! - **Catalogue.** One browsable provider per record of the workspace
//!   (`record:<app>:<record>`, the token agents use) and the workspace's own
//!   provider (`workspace:<workspace>`) offering `app.<app>#open`. A query naming
//!   no provider lists rows for the records open on the workspace's shared
//!   desktop, the most recently changed next, up to [`DEFAULT_ROWS_RECORDS`];
//!   naming a record's provider lists every action that record publishes.
//! - **Invoke.** The record is held as a replica of its resource in its own
//!   confined app worker ([`ConfinedReplica`]) from what the service opened
//!   of it, the operation runs there through the resource's own door
//!   (permission, confirmation and argument binding exactly as for a person),
//!   and the change it returns is admitted through the isolate, then sealed
//!   and signed here as the host device's own, so every page receives it as
//!   an ordinary durable change. No app code runs in this process, beside
//!   the host's key ([`crate::worker`]).
//! - **Tickets.** Invocation ids are idempotent: a retry reports the original
//!   invocation, a different request under a used id is a conflict, and a
//!   refused invocation is forgotten (nothing ran). An invocation is
//!   accepted at once and runs on the service's own thread; `Observe`
//!   reports where it stands, and `Cancel` stops one that has not started or
//!   ends the shell command of one that runs ([`terminal`]).
//!
//! Document invocations serve the host user's own devices as theirs. Anyone
//! else the workspace admits, and an agent a person's own device handed its
//! session to (the host's own person's included), is served within bounds
//! ([`bounded`]): in a replica of their own on their current standing, an
//! agent's bounded as every agent's is; what that replica offers is both
//! what their listing shows and all the host runs for them; an action that
//! asks first is put to the person; and nothing is saved that their standing
//! does not let them write. Exact local View discovery also serves currently
//! certified workspace readers, through the host's canonical Read policy;
//! that grants no document invocation authority.
//!
//! The service joins each workspace as the host's own device ([`WorkspaceMember`], from
//! the [`WorkspaceRecordsLink`] the host supplies): a member of the workspace's
//! channels like any other, admitted by the workspace's participants. It reads
//! only records it holds keys for and writes nothing in the clear. When the
//! host device itself takes part in the workspace's channels (a workspace the host
//! governs), it cannot be admitted to them a second time; the host hands the
//! service the keys it holds instead
//! ([`WorkspaceRecordsLink::host_keys`]), for each record a request reads or
//! acts on, fresh for every act.

mod bounded;
mod content;
mod feedback;
mod reads;
mod terminal;
pub use content::MAX_PUT_BYTES;
pub use terminal::{ShellRan, TERMINAL_APP, Watching, WorkspaceShell};

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;

use crate::apps::{AppCatalogue, Seed};
use crate::host::{CatalogueFactory, HostEndpoint};
use crate::participant::SharedDesktop;
use crate::participant::actions::{Outcome, door_outcome, open_row, record_of, record_provider};
use crate::worker::{ConfinedReplica, ConfinedViews};
use collab_types::DocumentMutation;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, ActorPlace, Caller,
    CallerPrincipal, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow, Completer,
    Confirmation, ConfirmationQuestion, ConfirmationReply, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    application_provider, application_provider_record, pending, provider_workspace,
    workspace_provider,
};
use tilo_actions::{
    ActionFailure, ActionOutput, ConfirmationPolicy, InvocationId, InvocationUpdate, PROMPTED_PARAM,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::room_workspace_id;
use tilo_session_host::{MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle};
use tilo_user_identity::RevocationList;
use ui::permissions::policy::{ActionKind, Answer, TreePath};

/// How long a comment the host files for someone may take to be saved
/// before the action reports that it was not.
const COMMIT_WAIT: Duration = Duration::from_secs(10);

/// Records whose rows a catalogue naming no provider lists.
pub const DEFAULT_ROWS_RECORDS: usize = 12;
/// Records kept held between requests; the least recently used is dropped.
const MAX_MOUNTED: usize = 24;
/// How long a request waits for the service to join a workspace.
const JOIN_WAIT: Duration = Duration::from_secs(30);
/// How long a request waits, once the workspace is joined, for the keys of what
/// it reads: the record it names, or (once per workspace) every record whose
/// keys are on their way. A named record whose keys have not arrived by
/// then is refused, never acted on half seen.
const KEYS_WAIT: Duration = Duration::from_secs(10);
/// Invocations remembered for retries, status and history.
const REMEMBERED: usize = 1024;

/// How the service takes part in the host's workspaces: as the host's own
/// device, a member of each workspace's channels like any other. It opens only
/// what a workspace's participants admitted it to and seals everything it writes.
#[derive(Clone)]
pub struct WorkspaceMember {
    /// The host's endpoint and certificate; the member is the host device.
    pub endpoint: HostEndpoint,
    /// The host device's key, which signs the member's changes.
    pub key: SigningKey,
    /// The runtime the member's sessions run on.
    pub runtime: tokio::runtime::Handle,
}

/// Current governed read policy supplied by the authenticated host boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewReadPolicy {
    /// Governed workspace whose current policy authorized the exact source record.
    pub workspace: String,
    /// The certified caller's current standing in the workspace, including the
    /// bound of a device that is not theirs.
    pub standing: ui::permissions::Standing,
    /// The workspace's current sandbox capabilities.
    pub sandbox: Vec<collab_types::caps::CapOverride>,
    /// Host time used to evaluate capability expiry.
    pub unix_time: u64,
}

impl ViewReadPolicy {
    pub(crate) fn permissions(&self) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::new(
            ui::permissions::Actor::person(self.standing.person.clone())
                .in_workspace(self.workspace.clone()),
            self.sandbox.clone(),
            self.standing.clone(),
        )
        .at_unix_time(self.unix_time)
    }
}

/// An existing verified source snapshot for internal view discovery only.
pub struct ViewDocument {
    /// Replayed snapshot; never returned in the public metadata response.
    pub bytes: Vec<u8>,
    /// Content revision for catalogue freshness.
    pub revision: String,
}

/// What the service needs of its host. Methods block; they are called from
/// the service's own thread, never from the host's runtime.
pub trait WorkspaceRecordsLink: Send + Sync {
    /// The user root the host chains to: the person whose device this service is.
    fn user_public_key(&self) -> [u8; 32];
    /// How the service joins the host's workspaces.
    fn member(&self) -> WorkspaceMember;
    /// Whether `workspace` admits `caller`, on the device it speaks for: a
    /// person the workspace lets in, as its governance records them, and the
    /// person the host serves itself. Anyone else is a stranger to it.
    fn admits(&self, _workspace: u64, _caller: &Caller) -> bool {
        false
    }
    /// Current certified-device admission and exact record Read capability.
    /// Absence denies; this never inherits document invocation authority.
    fn view_read_policy(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        None
    }
    /// The certified caller's standing across `workspace` now, with no
    /// record's read checked: what its content and its new records are
    /// decided by. Absence denies.
    fn workspace_policy(&self, _workspace: u64, _caller: &Caller) -> Option<ViewReadPolicy> {
        None
    }
    /// Read an exact existing source using already-owned host keys. The host
    /// must check current certified Read under its workspace lock before key access
    /// and again before returning. Missing keys never cause enrollment.
    fn view_document(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        Err("existing view documents unavailable".into())
    }
    /// The content keys of `route`'s channel, when the host device itself
    /// takes part in the channels of `route`'s workspace: it is then already the
    /// channel's member, and the service, which joins as that device, can
    /// only be handed the keys the host holds. `create` opens the channel of
    /// a record the service is about to write for the first time.
    ///
    /// `None` when the host does not take part in the workspace's channels: the
    /// service waits for a participant to admit it over the channel, like
    /// any member.
    /// `Some(Err)` when the host refused the hand-off or holds no such
    /// channel. Called only on behalf of a caller the service serves.
    fn host_keys(
        &self,
        _route: &DocumentRoute,
        _create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        None
    }
    /// The listener-owned public locator; absence cannot mint a browser link.
    fn view_locator(
        &self,
        _workspace: &str,
        _provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        None
    }
    /// Whether the bot installed as `installation` in `workspace` may show
    /// other names on what it writes. Absence refuses.
    fn bot_personas(&self, _workspace: u64, _installation: &str) -> bool {
        false
    }
    /// Where the workspaces' Terminal sessions run their shells on this
    /// host ([`WorkspaceShell`]). Absence leaves a session's `run` to the
    /// device running the session.
    fn shell(&self) -> Option<Arc<dyn WorkspaceShell>> {
        None
    }
    /// The bytes `workspace`'s content store holds under `hash`, for an
    /// avatar an appearance names. Absence refuses the appearance.
    fn content_blob(&self, _workspace: u64, _hash: &str) -> Option<Vec<u8>> {
        None
    }
    /// `workspace`'s content store on this host: where a caller's uploads
    /// go, what the record workers it runs actions in are lent, and what
    /// the host answers the workspace's members from. Absence leaves the
    /// workers without one and refuses uploads.
    fn workspace_content(&self, _workspace: u64) -> Option<tilo_content::ContentStore> {
        None
    }
    /// The model the host looks over its reporters' reviews with, when it
    /// lends one ([`FeedbackReviewer`]). Absence leaves reviews to the
    /// reporters' own devices.
    fn feedback_reviewer(&self) -> Option<Arc<dyn FeedbackReviewer>> {
        None
    }
    /// Put `question` about one invocation the host runs in `workspace` to
    /// the person `caller` is or acts for, on their own devices, and answer
    /// what they said. `None` when this host has nobody to ask, which leaves
    /// the invocation refused: nobody else confirms for them.
    fn ask_person(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionHostError>>> {
        None
    }
}

/// A model the host lends its admitted reporters to look over a report's
/// review: it is asked only the feedback review contract
/// (`ui::feedback::model_review`) over the report's model view, which shows
/// personal values only as placeholders, and only quotes from the texts
/// asked about go back.
pub trait FeedbackReviewer: Send + Sync {
    /// What a review calls the model: the provider and model.
    fn name(&self) -> String;
    /// Ask the model `prompt` for `caller`, its answer held to the JSON
    /// `schema` where the provider can, then call `done` with its
    /// answer, from any thread. A host may refuse a caller who asks too
    /// often.
    fn review(
        &self,
        caller: &Caller,
        prompt: String,
        schema: serde_json::Value,
        done: Box<dyn FnOnce(Result<String, String>) + Send>,
    );
}

struct Entry {
    caller: Caller,
    request: InvokeRequest,
    update: InvocationUpdate,
    /// Where the invocation left its actor in the record it acted on.
    left_at: Option<ActorPlace>,
    /// Set while the host's shell runs this invocation: what a cancel of it,
    /// or a Ctrl+C in the session it runs in, stops.
    stop: Option<Arc<AtomicBool>>,
    /// The person was asked to allow it ([`bounded`]): it was answered
    /// accepted, and settles as failed rather than being forgotten.
    asked: bool,
    /// The token naming the person's answer, once they allowed it.
    witness: Option<String>,
}

#[derive(Default)]
struct Ledger {
    entries: HashMap<InvocationId, Entry>,
    order: VecDeque<InvocationId>,
    running: Option<InvocationId>,
}

impl Ledger {
    fn insert(&mut self, caller: &Caller, request: InvokeRequest) {
        while self.order.len() >= REMEMBERED {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        let invocation = request.invocation;
        self.order.push_back(invocation);
        self.entries.insert(
            invocation,
            Entry {
                caller: caller.clone(),
                request,
                update: InvocationUpdate::Accepted { invocation },
                left_at: None,
                stop: None,
                asked: false,
                witness: None,
            },
        );
    }

    fn forget(&mut self, id: &InvocationId) {
        self.entries.remove(id);
        self.order.retain(|candidate| candidate != id);
    }

    fn owned(&self, caller: &Caller, id: &InvocationId) -> Option<&Entry> {
        self.entries.get(id).filter(|entry| entry.caller == *caller)
    }
}

enum Job {
    Feedback {
        workspace_id: u64,
        query: Box<tilo_actions::remote::feedback::FeedbackQuery>,
        caller: Caller,
        done: Completer<Result<tilo_actions::remote::feedback::FeedbackAnswer, ActionHostError>>,
    },
    Catalogue {
        workspace_id: u64,
        query: Box<CatalogueQuery>,
        caller: Caller,
        done: Completer<Result<CataloguePage, ActionHostError>>,
    },
    Invoke {
        workspace_id: u64,
        invocation: InvocationId,
        done: Completer<InvocationReceipt>,
    },
    /// A Terminal session's run that ended in the host's shell, to record.
    Ran(Box<terminal::Ended>),
    /// A step of an upload into a workspace whose store the service has not
    /// found yet.
    Content {
        workspace_id: u64,
        caller: Caller,
        step: tilo_actions::remote::ContentPutStep,
        done: Completer<Result<tilo_actions::remote::ContentPutAnswer, ActionHostError>>,
    },
    /// A piece of content asked of a workspace whose store the service has
    /// not found yet.
    ContentGet {
        workspace_id: u64,
        caller: Caller,
        content: String,
        offset: u64,
        length: u64,
        done: Completer<Result<tilo_actions::remote::ContentGetAnswer, ActionHostError>>,
    },
}

impl Job {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Feedback { .. } => "feedback",
            Self::Catalogue { .. } => "catalogue",
            Self::Invoke { .. } => "invoke",
            Self::Ran(_) => "ran",
            Self::Content { .. } => "content",
            Self::ContentGet { .. } => "content-get",
        }
    }

    /// The workspace whose records the job reads or changes.
    const fn workspace(&self) -> Option<u64> {
        match self {
            Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. } => Some(*workspace_id),
            Self::Feedback { .. } | Self::Ran(_) => None,
        }
    }
}

/// A request that waited for the worker, or ran, at least this long is
/// logged: every request of every page of every workspace waits behind the one
/// before it.
const SLOW_JOB: Duration = Duration::from_secs(2);

struct Shared {
    ledger: Mutex<Ledger>,
    link: OnceLock<Arc<dyn WorkspaceRecordsLink>>,
    jobs: Mutex<Option<mpsc::Sender<(Instant, Job)>>>,
    /// How the service joins workspaces, once its thread runs.
    member: OnceLock<WorkspaceMember>,
    /// Every workspace a request named, by id: joined as the request
    /// arrives, while the service's one thread still answers others, so the
    /// first requests of many fresh workspaces join side by side instead of
    /// each waiting for the joins before it. `Some` until the thread takes
    /// the membership over.
    joins: Mutex<HashMap<u64, Option<Workspace>>>,
    /// Each workspace's content store, once found.
    stores: content::Stores,
    /// The uploads callers have open.
    puts: tilo_actions::remote::HostedContentPuts,
    /// The answers people gave to invocations run for them.
    witnesses: bounded::Witnesses,
}

/// The host's page actions over every record of its workspaces.
#[derive(Clone)]
pub struct WorkspaceActions {
    shared: Arc<Shared>,
    catalogue: CatalogueFactory,
}

impl WorkspaceActions {
    /// A service mounting apps from `catalogue`; it answers once
    /// [`Self::attach`] gives it the host's records.
    #[must_use]
    pub fn new(catalogue: CatalogueFactory) -> Self {
        Self {
            shared: Arc::new(Shared {
                ledger: Mutex::new(Ledger::default()),
                link: OnceLock::new(),
                jobs: Mutex::new(None),
                member: OnceLock::new(),
                joins: Mutex::default(),
                stores: Mutex::default(),
                puts: tilo_actions::remote::HostedContentPuts::default(),
                witnesses: bounded::Witnesses::default(),
            }),
            catalogue,
        }
    }

    /// Start serving `link`'s workspaces on the service's own thread. A second
    /// call is ignored.
    pub fn attach(&self, link: Arc<dyn WorkspaceRecordsLink>) -> Result<(), String> {
        if self.shared.link.set(Arc::clone(&link)).is_err() {
            return Ok(());
        }
        let (sender, jobs) = mpsc::channel();
        let shared = Arc::clone(&self.shared);
        let catalogue = Arc::clone(&self.catalogue);
        std::thread::Builder::new()
            .name("workspace-actions".into())
            .spawn(move || Worker::new(link.as_ref(), catalogue(), shared).run(&jobs))
            .map_err(|error| error.to_string())?;
        *self.shared.jobs.lock_recover() = Some(sender);
        Ok(())
    }

    /// Stop the service thread; later requests are refused as unavailable.
    pub fn shutdown(&self) {
        self.shared.jobs.lock_recover().take();
    }

    /// Whether `caller` is the host's own person, or an agent one of their
    /// devices handed its session to.
    fn authorized(&self, caller: &Caller) -> bool {
        let Some(link) = self.shared.link.get() else {
            return false;
        };
        acts_for(caller, &hex::encode(link.user_public_key()))
    }

    /// Whether `caller` may ask the host to run `request` at all. Anyone the
    /// workspace admits may ask for a resource's link: it reads where the resource
    /// is shown and grants nothing, and the governed view lookup behind it
    /// applies their own grant. Other people and their agents may act on a
    /// record, or open a new one, through the host: the worker runs it under
    /// their own current standing, and refuses what it does not allow.
    fn admits(&self, caller: &Caller, request: &InvokeRequest) -> bool {
        let on_record = record_of(&request.target.provider, "").is_some();
        let link_request =
            request.target.address.action == ui::tilo_resource::share::SHARE_LINK && on_record;
        let on_workspace = provider_workspace(&request.target.provider).is_some();
        let member_request = (on_record || on_workspace) && speaks_for_a_member(caller);
        link_request || member_request || self.authorized(caller)
    }

    /// Start joining `workspace_id` unless the service has: the thread
    /// waits only for what is still missing when it reaches the request.
    /// Joining sends the subscription and returns; the session runs on the
    /// member's runtime.
    fn join_ahead(&self, workspace_id: u64) {
        let Some(member) = self.shared.member.get() else {
            return;
        };
        let mut joins = self.shared.joins.lock_recover();
        if joins.contains_key(&workspace_id) {
            return;
        }
        match Workspace::join(member, workspace_id) {
            Ok(workspace) => {
                joins.insert(workspace_id, Some(workspace));
                drop(joins);
            }
            Err(error) => {
                drop(joins);
                tracing::debug!(%error, workspace_id, "workspace actions: a workspace was not joined ahead");
            }
        }
    }

    /// Whether `caller` started invocation `id` here.
    #[must_use]
    pub fn owns(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.ledger().owned(caller, id).is_some()
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.shared.ledger.lock_recover()
    }

    #[allow(
        clippy::result_large_err,
        reason = "a job the worker cannot take goes back to its sender whole"
    )]
    fn submit(&self, job: Job) -> Result<(), Job> {
        if let Some(workspace_id) = job.workspace() {
            self.join_ahead(workspace_id);
        }
        let jobs = self.shared.jobs.lock_recover();
        match jobs.as_ref() {
            Some(sender) => sender
                .send((Instant::now(), job))
                .map_err(|error| error.0.1),
            None => Err(job),
        }
    }

    /// Answer one request `caller` sent to the host for `workspace_id`, or
    /// `None` for a message that is not a request.
    pub fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let unavailable = || ActionHostError::Unavailable {
            reason: "the host's workspace actions are not running".into(),
        };
        Some(match message {
            RemoteActionMessage::FeedbackRequest { correlation, query } => {
                self.feedback_request(workspace_id, caller, correlation, query)
            }
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let respond = move |result| RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result,
                };
                // A person is answered by the service's own thread, which
                // checks that the workspace admits them; nobody else is
                // answered at all. The check runs there because asking the host
                // blocks and this is the host's runtime.
                if query.view_resource.is_none() && !speaks_for_a_member(caller) {
                    let empty = CataloguePage::from_rows("0".into(), Vec::new(), &query);
                    return Some(Pending::ready(respond(Ok(empty))));
                }
                let (done, answer) = pending();
                let job = Job::Catalogue {
                    workspace_id,
                    query: Box::new(query),
                    caller: caller.clone(),
                    done,
                };
                if let Err(Job::Catalogue { done, .. }) = self.submit(job) {
                    done.complete(Err(unavailable()));
                }
                answer.map(respond)
            }
            RemoteActionMessage::Invoke(request) => {
                self.invoke_request(workspace_id, caller, request)
            }
            RemoteActionMessage::Observe { invocation, .. } => {
                let state = self.ledger().owned(caller, &invocation).map(|entry| {
                    InvocationState::new(invocation, entry.update.clone())
                        .left_at(entry.left_at.clone())
                });
                Pending::ready(RemoteActionMessage::Update { invocation, state })
            }
            RemoteActionMessage::Cancel { invocation } => {
                Pending::ready(RemoteActionMessage::CancelResponse {
                    invocation,
                    result: self.cancel(caller, &invocation),
                })
            }
            RemoteActionMessage::ContentPutRequest { correlation, step } => {
                self.content_put(workspace_id, caller, correlation, step)
            }
            RemoteActionMessage::ContentGetRequest {
                correlation,
                content,
                offset,
                length,
            } => self.content_get(workspace_id, caller, correlation, (content, offset, length)),
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RecordRequest { .. }
            | RemoteActionMessage::RecordResponse { .. }
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::RetiredResourceWatch { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return None,
        })
    }

    /// Admit `request` from `caller` and run it on the service's thread,
    /// answering its receipt.
    fn invoke_request(
        &self,
        workspace_id: u64,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        // Answered at admission: a wait is ignored, as an older host
        // would, and is no part of the remembered request.
        request.wait_ms = None;
        let invocation = request.invocation;
        let receipt = |status| {
            Pending::ready(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status,
            }))
        };
        if !self.admits(caller, &request) {
            return receipt(ReceiptStatus::Rejected {
                error: ActionHostError::UnknownAction,
            });
        }
        {
            let mut ledger = self.ledger();
            if let Some(entry) = ledger.entries.get(&invocation) {
                if entry.caller != *caller || !entry.request.same_invocation(&request) {
                    return receipt(ReceiptStatus::Rejected {
                        error: ActionHostError::InvocationConflict,
                    });
                }
                let state = InvocationState::new(invocation, entry.update.clone());
                return receipt(state.receipt().status);
            }
            ledger.insert(caller, request);
        }
        let (done, answer) = pending();
        let job = Job::Invoke {
            workspace_id,
            invocation,
            done,
        };
        if let Err(Job::Invoke { done, .. }) = self.submit(job) {
            self.ledger().forget(&invocation);
            done.complete(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::Unavailable {
                        reason: "the host's workspace actions are not running".into(),
                    },
                },
            });
        }
        answer.map(RemoteActionMessage::Receipt)
    }

    fn feedback_request(
        &self,
        workspace_id: u64,
        caller: &Caller,
        correlation: u64,
        query: tilo_actions::remote::feedback::FeedbackQuery,
    ) -> Pending<RemoteActionMessage> {
        let (done, answer) = pending();
        let job = Job::Feedback {
            workspace_id,
            query: Box::new(query),
            caller: caller.clone(),
            done,
        };
        if let Err(Job::Feedback { done, .. }) = self.submit(job) {
            done.complete(Err(ActionHostError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(move |result| RemoteActionMessage::FeedbackResponse {
            correlation,
            result,
        })
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        let mut ledger = self.ledger();
        let running = ledger.running == Some(*id);
        let entry = ledger
            .entries
            .get_mut(id)
            .filter(|entry| entry.caller == *caller)
            .ok_or(ActionHostError::UnknownInvocation)?;
        if entry.update.is_terminal() {
            return Err(ActionHostError::NotCancellable {
                reason: "the invocation has finished".into(),
            });
        }
        // A run in the host's shell ends there, as on a Ctrl+C, and settles
        // as cancelled once what it printed is recorded.
        if let Some(stop) = &entry.stop {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        if running {
            return Err(ActionHostError::NotCancellable {
                reason: "the action is already running".into(),
            });
        }
        entry.update = InvocationUpdate::Cancelled {
            invocation: *id,
            message: Some("cancelled before it started".into()),
        };
        drop(ledger);
        Ok(())
    }
}

/// The host's whole app-serving plane: the agent supervisor's vocabulary
/// (provider `agents`) and the workspace's page actions (everything else).
pub struct HostActions {
    /// Answers `agents` requests.
    pub supervisor: crate::control::SupervisorHost,
    /// Answers requests about records and the workspace.
    pub actions: WorkspaceActions,
}

impl HostActions {
    /// Answer one request `caller` sent to the host for `workspace_id`, or
    /// `None` for a message that is not a request.
    pub fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        use tilo_relay_protocol::agents::AGENTS_PROVIDER;
        let supervisor = match &message {
            RemoteActionMessage::FeedbackRequest { .. }
            | RemoteActionMessage::ContentPutRequest { .. }
            | RemoteActionMessage::ContentGetRequest { .. } => false,
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(AGENTS_PROVIDER)
            }
            RemoteActionMessage::Invoke(request) => request.target.provider == AGENTS_PROVIDER,
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => !self.actions.owns(caller, invocation),
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RecordRequest { .. }
            | RemoteActionMessage::RecordResponse { .. }
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::RetiredResourceWatch { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return None,
        };
        if supervisor {
            tilo_actions::remote::serve(&self.supervisor, caller, message)
        } else {
            self.actions.serve(workspace_id, caller, message)
        }
    }
}

/// One record held on the service thread, in its own app worker.
struct Mounted {
    app: ConfinedReplica,
    /// Digests of the bodies already merged into it.
    seen: HashSet<[u8; 32]>,
    /// The records held beside it (the parts of a chain its app keeps), by
    /// route, with the digests of their bodies already merged.
    parts: HashMap<DocumentRoute, HashSet<[u8; 32]>>,
    used: u64,
}

/// Whether the wire record `record` is a part of a chain an app keeps
/// beside its own record ([`ui::record::RecordId::part`]): held with that
/// record, never on its own.
fn is_part(record: &str) -> bool {
    ui::record::RecordId::parse(record).is_some_and(|id| id.chain_part().is_some())
}

/// A record's whole document, from its changes in order.
fn whole(changes: &[Change]) -> Result<Vec<u8>, String> {
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in changes {
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes)
            .map_err(|error| error.to_string())?;
    }
    Ok(doc.save())
}

/// One change of a record: a digest naming it, and its bytes.
type Change = ([u8; 32], Vec<u8>);

#[derive(Default)]
struct WorkspaceState {
    workspace: Option<String>,
    /// Each record's changes in the order the member took them.
    records: BTreeMap<DocumentRoute, Vec<Change>>,
    /// When each record last changed, as a position in that order.
    changed: HashMap<DocumentRoute, usize>,
    /// The records open on the workspace's shared desktop.
    on_desktop: HashSet<String>,
    revision: usize,
}

/// What a request must have opened before it reads the workspace.
#[derive(Clone, Copy, Debug)]
enum Needs<'a> {
    /// The workspace joined and nothing sealed: a new record of the workspace.
    Joined,
    /// The workspace listed: every record whose keys are on their way, waited for
    /// once per workspace (the first listing after the join). Later listings show
    /// what is open and wait for nothing.
    Workspace,
    /// One record's actions listed by name: its keys, waited for once. A
    /// listing that gave up on them lists what opened and is not held back
    /// by them again.
    Read { app_id: &'a str, record: &'a str },
    /// One record acted on: its keys, every time. Nothing acts on a record
    /// seen only in part.
    Act { app_id: &'a str, record: &'a str },
}

/// What a workspace's earlier waits for keys gave up on.
#[derive(Default)]
struct GaveUp {
    listed: bool,
    /// Records a listing by name waited for in vain.
    records: HashSet<DocumentRoute>,
}

/// The routes of `workspace_id` in `routes` still undecided for `needs`:
/// documents that arrived and open only once the member holds their keys, or
/// has the surface they were written with and their workspace's governance.
fn sealed_for(
    routes: &BTreeMap<DocumentRoute, tilo_session_host::session::RouteStatus>,
    workspace_id: u64,
    needs: Needs<'_>,
    gave_up: &GaveUp,
) -> Vec<DocumentRoute> {
    // A part of a chain is held beside a record of its app that keeps it:
    // reading or acting on it needs that record open too.
    let named = |route: &DocumentRoute, app_id: &str, record: &str| {
        route.app_id == app_id
            && (route.record == record || (is_part(record) && !is_part(&route.record)))
    };
    routes
        .iter()
        .filter(|(route, status)| {
            status.waiting() > 0
                && room_workspace_id(&route.cluster_id).ok() == Some(workspace_id)
                && match needs {
                    Needs::Joined => false,
                    Needs::Workspace => !gave_up.listed,
                    Needs::Read { app_id, record } => {
                        named(route, app_id, record) && !gave_up.records.contains(*route)
                    }
                    Needs::Act { app_id, record } => named(route, app_id, record),
                }
        })
        .map(|(route, _)| route.clone())
        .collect()
}

/// The service's membership of one workspace.
struct Workspace {
    peer: PeerHandle,
    events: PeerEvents,
    /// Signalled whenever the session's status or events change.
    wake: Arc<(Mutex<bool>, Condvar)>,
    ready: bool,
    /// What earlier waits for keys gave up on.
    gave_up: GaveUp,
    /// Every change the member opened or wrote, in order.
    log: Vec<(DocumentRoute, Change)>,
    /// The shared desktop of the workspace the log names first, merged as its
    /// changes arrive; they are admitted only through the isolate.
    desktop: SharedDesktop,
}

impl Workspace {
    fn join(member: &WorkspaceMember, workspace_id: u64) -> Result<Self, String> {
        let _runtime = member.runtime.enter();
        let peer = PeerHandle::start(
            PeerConfig {
                host: member.endpoint.host.clone(),
                cert: member.endpoint.host_cert.clone(),
                host_cert: member.endpoint.host_cert.clone(),
                revocations: RevocationList::new(),
            },
            Arc::new(member.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = Arc::clone(&wake);
        peer.on_change(Arc::new(move || {
            let (flag, condvar) = &*signal;
            *flag.lock_recover() = true;
            condvar.notify_all();
        }));
        peer.join(workspace_id)?;
        Ok(Self {
            peer,
            events,
            wake,
            ready: false,
            gave_up: GaveUp::default(),
            log: Vec::new(),
            desktop: SharedDesktop::new(),
        })
    }

    fn pump(&mut self, workspace_id: u64) {
        for event in self.events.drain() {
            match event {
                PeerEvent::Ready {
                    workspace_id: ready,
                } if ready == workspace_id => {
                    self.ready = true;
                }
                PeerEvent::Document(document) => {
                    if room_workspace_id(&document.route.cluster_id).ok() != Some(workspace_id) {
                        continue;
                    }
                    let Some(mutation) = document.mutation() else {
                        continue;
                    };
                    let bytes = bytes_of(&mutation);
                    let digest = change_digest(&document.author, &bytes);
                    let workspace = self
                        .log
                        .first()
                        .map_or(&document.route.cluster_id, |(first, _)| &first.cluster_id);
                    if document.route == crate::participant::desktop_route(workspace) {
                        self.desktop.receive(bytes.clone());
                    }
                    self.log.push((document.route, (digest, bytes)));
                }
                _ => {}
            }
        }
    }

    /// The records of the workspace that `needs` reads and that hold
    /// undecided documents: they open once what the documents wait for
    /// arrives.
    fn sealed(&self, workspace_id: u64, needs: Needs<'_>) -> Vec<DocumentRoute> {
        sealed_for(
            &self.peer.status().routes,
            workspace_id,
            needs,
            &self.gave_up,
        )
    }

    /// Join, then wait for the keys of what `needs` reads. Every wait is
    /// woken by the session (a key arriving wakes it), never polled.
    ///
    /// A record to act on whose keys do not arrive within [`KEYS_WAIT`] is
    /// refused: nothing acts on a record seen only in part. A listing gives
    /// up quietly, lists what opened, and does not wait for the same keys
    /// again.
    fn wait_for(
        &mut self,
        workspace_id: u64,
        needs: Needs<'_>,
        link: Option<&dyn WorkspaceRecordsLink>,
    ) -> Result<(), ActionHostError> {
        let joined = Instant::now()
            .checked_add(JOIN_WAIT)
            .unwrap_or_else(Instant::now);
        self.settle(workspace_id, joined, |workspace| workspace.ready);
        if !self.ready {
            return Err(ActionHostError::Unavailable {
                reason: "the host has not joined the workspace yet".into(),
            });
        }
        let keyed = Instant::now()
            .checked_add(KEYS_WAIT)
            .unwrap_or_else(Instant::now);
        // Keys the host device holds are handed over as soon as a
        // route needs them: the record acted on at once (fresh every time),
        // and every route that arrives sealed while this waits.
        let mut handed = HashSet::new();
        if let Some(link) = link
            && let Needs::Act { app_id, record } = needs
            && let Some(workspace) = self.cluster_id(workspace_id)
        {
            let route = DocumentRoute {
                app_id: app_id.to_owned(),
                cluster_id: workspace,
                record: record.to_owned(),
            };
            self.take_host_keys(vec![route], needs, link, &mut handed)?;
        }
        loop {
            self.settle(workspace_id, keyed, |workspace| {
                let sealed = workspace.sealed(workspace_id, needs);
                sealed.is_empty()
                    || (link.is_some() && sealed.iter().any(|route| !handed.contains(route)))
            });
            let unhanded: Vec<DocumentRoute> = self
                .sealed(workspace_id, needs)
                .into_iter()
                .filter(|route| !handed.contains(route))
                .collect();
            match link {
                Some(link) if !unhanded.is_empty() => {
                    self.take_host_keys(unhanded, needs, link, &mut handed)?;
                }
                _ => break,
            }
        }
        let sealed = self.sealed(workspace_id, needs);
        match needs {
            Needs::Act { app_id, record } if !sealed.is_empty() => {
                Err(ActionHostError::Unavailable {
                    reason: format!(
                        "the host does not hold the keys to {app_id} record {record} yet"
                    ),
                })
            }
            Needs::Workspace | Needs::Read { .. } => {
                if !sealed.is_empty() {
                    tracing::info!(
                        records = sealed.len(),
                        "workspace actions: listing without what has not opened: its keys have not arrived"
                    );
                }
                if matches!(needs, Needs::Workspace) {
                    self.gave_up.listed = true;
                }
                self.gave_up.records.extend(sealed);
                Ok(())
            }
            Needs::Joined | Needs::Act { .. } => Ok(()),
        }
    }

    /// Hold the keys the host device hands over for `routes`
    /// ([`WorkspaceRecordsLink::host_keys`]), noting each in `handed`. The
    /// session then opens what it held for them and wakes the wait. A record
    /// to act on whose keys the host refuses is refused at once; a listing
    /// goes on without it.
    fn take_host_keys(
        &self,
        routes: Vec<DocumentRoute>,
        needs: Needs<'_>,
        link: &dyn WorkspaceRecordsLink,
        handed: &mut HashSet<DocumentRoute>,
    ) -> Result<(), ActionHostError> {
        for route in routes {
            handed.insert(route.clone());
            match link.host_keys(&route, false) {
                None => {}
                Some(Ok(keys)) => {
                    self.peer
                        .install_channel_keys(route, keys)
                        .map_err(|reason| ActionHostError::Unavailable { reason })?;
                }
                Some(Err(reason)) => {
                    if let Needs::Act { app_id, record } = needs {
                        return Err(ActionHostError::Unavailable {
                            reason: format!(
                                "the host holds no keys to {app_id} record {record}: {reason}"
                            ),
                        });
                    }
                    tracing::info!(app = %route.app_id, record = %route.record, %reason,
                        "workspace actions: listing without a record the host holds no keys to");
                }
            }
        }
        Ok(())
    }

    /// The cluster id of the workspace `workspace_id` names, from what the member has
    /// seen of it.
    fn cluster_id(&self, workspace_id: u64) -> Option<String> {
        let of_workspace =
            |route: &DocumentRoute| room_workspace_id(&route.cluster_id).ok() == Some(workspace_id);
        if let Some((route, _)) = self.log.iter().find(|(route, _)| of_workspace(route)) {
            return Some(route.cluster_id.clone());
        }
        self.peer
            .status()
            .routes
            .into_keys()
            .find(|route| of_workspace(route))
            .map(|route| route.cluster_id)
    }

    /// Take the session's events until `settled`, or until `deadline`.
    fn settle(&mut self, workspace_id: u64, deadline: Instant, settled: impl Fn(&Self) -> bool) {
        loop {
            {
                let (flag, _) = &*self.wake;
                *flag.lock_recover() = false;
            }
            self.pump(workspace_id);
            if settled(self) {
                // The session queues a change's events before the status
                // that reports it: take what arrived since the pump above.
                self.pump(workspace_id);
                return;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            let (flag, condvar) = &*self.wake;
            let guard = flag.lock_recover();
            let _ = condvar
                .wait_timeout_while(guard, left, |changed| !*changed)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn bytes_of(mutation: &DocumentMutation) -> Vec<u8> {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes.clone(),
    }
}

fn change_digest(author: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(author);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

struct Worker {
    member: WorkspaceMember,
    catalogue: AppCatalogue,
    shared: Arc<Shared>,
    participant: String,
    workspaces: HashMap<u64, Workspace>,
    mounted: BTreeMap<DocumentRoute, Mounted>,
    /// The replicas callers served within bounds act through ([`bounded`]).
    bounded: BTreeMap<bounded::BoundedKey, bounded::BoundedReplica>,
    /// The route of the mounted record each part is held beside.
    beside: HashMap<DocumentRoute, DocumentRoute>,
    clock: u64,
}

/// The door's answer for an action it has nothing by that address.
fn unknown_action() -> Outcome {
    Outcome::Rejected(ActionHostError::UnknownAction)
}

/// Whether `caller` is `person`, or an agent a device of theirs handed its
/// session to.
fn acts_for(caller: &Caller, person: &str) -> bool {
    match caller.principal() {
        CallerPrincipal::Person(id) => id == person,
        CallerPrincipal::Agent(_) => caller.delegating_person().is_some_and(|delegating| {
            delegating.principal() == &CallerPrincipal::Person(person.to_owned())
        }),
        CallerPrincipal::App(_) | CallerPrincipal::Bot(_) => false,
    }
}

/// Merge into `mounted` what `state` holds of its record and its parts that
/// it has not merged yet.
fn catch_up(route: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    if let Some(changes) = state.records.get(route) {
        for (digest, bytes) in changes {
            if mounted.seen.insert(*digest)
                && let Err(error) = mounted
                    .app
                    .apply(&DocumentMutation::Incremental(bytes.clone()))
            {
                tracing::warn!(%error, "workspace actions: a change did not merge");
            }
        }
    }
    for (part, seen) in &mut mounted.parts {
        let (Some(changes), Ok(record)) = (state.records.get(part), reads::record_id(&part.record))
        else {
            continue;
        };
        for (digest, bytes) in changes {
            if seen.insert(*digest)
                && let Err(error) = mounted
                    .app
                    .apply_record(&record, &DocumentMutation::Incremental(bytes.clone()))
            {
                tracing::warn!(%error, "workspace actions: a change to a part did not merge");
            }
        }
    }
}

/// What a held record publishes that its door runs now.
fn live_actions(app: &ConfinedReplica) -> Vec<tilo_actions::ActionDescriptor> {
    app.survey()
}

/// The outcome of an action whose change could not be committed.
fn commit_failed(message: String, retryable: bool) -> Outcome {
    Outcome::Failed(ActionFailure {
        code: "commit_failed".into(),
        message,
        details: None,
        retryable,
    })
}

/// Everything `app` changed since the last take, each with its route: the
/// held record's own (`None`: the holder's route) first, then each record
/// held beside it on the route of the same app and workspace its id names.
fn take_all(
    app: &ConfinedReplica,
    holder: &DocumentRoute,
) -> Result<Vec<(Option<DocumentRoute>, DocumentMutation)>, crate::apps::AppError> {
    let mut taken: Vec<(Option<DocumentRoute>, DocumentMutation)> = app
        .take_mutation()?
        .into_iter()
        .map(|mutation| (None, mutation))
        .collect();
    taken.extend(
        app.take_record_mutations()?
            .into_iter()
            .map(|(record, mutation)| {
                let route = DocumentRoute {
                    app_id: holder.app_id.clone(),
                    cluster_id: holder.cluster_id.clone(),
                    record: record.route().to_owned(),
                };
                (Some(route), mutation)
            }),
    );
    Ok(taken)
}

impl Worker {
    fn new(link: &dyn WorkspaceRecordsLink, catalogue: AppCatalogue, shared: Arc<Shared>) -> Self {
        let member = link.member();
        let _ = shared.member.set(member.clone());
        Self {
            participant: hex::encode(link.user_public_key()),
            member,
            catalogue,
            shared,
            workspaces: HashMap::new(),
            mounted: BTreeMap::new(),
            bounded: BTreeMap::new(),
            beside: HashMap::new(),
            clock: 0,
        }
    }

    fn run(mut self, jobs: &mpsc::Receiver<(Instant, Job)>) {
        while let Ok((queued, job)) = jobs.recv() {
            let started = Instant::now();
            let kind = job.kind();
            self.take(job);
            let (waited, ran) = (started.duration_since(queued), started.elapsed());
            if waited.max(ran) >= SLOW_JOB {
                tracing::warn!(
                    kind,
                    waited_ms = waited.as_millis(),
                    ran_ms = ran.as_millis(),
                    "workspace actions: a request waited or ran long"
                );
            }
        }
    }

    fn take(&mut self, job: Job) {
        {
            match job {
                Job::Feedback {
                    workspace_id,
                    query,
                    caller,
                    done,
                } => match self.shared.link.get() {
                    Some(link) => feedback::answer_and_review(
                        link,
                        self.catalogue.clone(),
                        workspace_id,
                        caller,
                        *query,
                        done,
                    ),
                    None => done.complete(Err(ActionHostError::Unavailable {
                        reason: "feedback host unavailable".into(),
                    })),
                },
                Job::Catalogue {
                    workspace_id,
                    query,
                    caller,
                    done,
                } => {
                    let result = if query.view_resource.is_some() {
                        self.view_catalogue_page(workspace_id, &caller, &query)
                    } else {
                        self.catalogue_page(workspace_id, &caller, &query)
                    };
                    done.complete(result);
                }
                Job::Invoke {
                    workspace_id,
                    invocation,
                    done,
                } => done.complete(self.invoke(workspace_id, invocation)),
                Job::Ran(ended) => self.ran(&ended),
                Job::Content {
                    workspace_id,
                    caller,
                    step,
                    done,
                } => done.complete(self.content_put(workspace_id, &caller, step)),
                Job::ContentGet {
                    workspace_id,
                    caller,
                    content,
                    offset,
                    length,
                    done,
                } => done.complete(self.content_get(
                    workspace_id,
                    &caller,
                    &content,
                    (offset, length),
                )),
            }
        }
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.shared.ledger.lock_recover()
    }

    /// The member's view of `workspace_id`, joining the workspace first and
    /// holding the keys of what `needs` reads, and every change the mounted
    /// records have not merged yet.
    fn refresh(
        &mut self,
        workspace_id: u64,
        needs: Needs<'_>,
    ) -> Result<WorkspaceState, ActionHostError> {
        if !self.workspaces.contains_key(&workspace_id) {
            let ahead = self
                .shared
                .joins
                .lock_recover()
                .insert(workspace_id, None)
                .flatten();
            let workspace = match ahead {
                Some(workspace) => workspace,
                None => Workspace::join(&self.member, workspace_id)
                    .map_err(|reason| ActionHostError::Unavailable { reason })?,
            };
            self.workspaces.insert(workspace_id, workspace);
        }
        let workspace =
            self.workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: "the workspace was not joined".into(),
                })?;
        workspace.wait_for(
            workspace_id,
            needs,
            self.shared.link.get().map(|link| &**link),
        )?;
        let mut state = WorkspaceState::default();
        for (position, (route, change)) in workspace.log.iter().enumerate() {
            state
                .workspace
                .get_or_insert_with(|| route.cluster_id.clone());
            state.changed.insert(route.clone(), position);
            state
                .records
                .entry(route.clone())
                .or_default()
                .push(change.clone());
        }
        state.revision = workspace.log.len();
        state.on_desktop.clone_from(workspace.desktop.tabs());
        for (route, mounted) in &mut self.mounted {
            catch_up(route, mounted, &state);
        }
        for ((route, _, _), held) in &mut self.bounded {
            catch_up(route, &mut held.held, &state);
        }
        Ok(state)
    }

    /// The record at `route`, mounted from what the workspace retains: a part of
    /// a chain held beside its app's own record, in that record's mount.
    fn mount(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<&mut Mounted, String> {
        let holder = if is_part(&route.record) {
            self.mount_part(route, state)?
        } else {
            self.mount_own(route, state)?;
            route.clone()
        };
        self.mounted
            .get_mut(&holder)
            .ok_or_else(|| "the record did not mount".to_string())
    }

    /// The route of the mounted record `route`'s record is held in: its own,
    /// or for a part, the record it is held beside.
    fn holder_of(&self, route: &DocumentRoute) -> DocumentRoute {
        self.beside.get(route).unwrap_or(route).clone()
    }

    /// Mount the part at `route` beside the record of its app that keeps it,
    /// mounting that record first. The holder's route.
    fn mount_part(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<DocumentRoute, String> {
        if let Some(holder) = self.beside.get(route).cloned()
            && self.mounted.contains_key(&holder)
        {
            self.mount_own(&holder, state)?;
            return Ok(holder);
        }
        let changes = state
            .records
            .get(route)
            .ok_or_else(|| format!("the workspace has no record {}", route.record))?;
        let record = reads::record_id(&route.record)?;
        let bytes = whole(changes)?;
        let holders: Vec<DocumentRoute> = state
            .records
            .keys()
            .filter(|held| {
                held.app_id == route.app_id
                    && held.cluster_id == route.cluster_id
                    && !is_part(&held.record)
            })
            .cloned()
            .collect();
        let mut refusals = Vec::new();
        for holder in holders {
            if let Err(error) = self.mount_own(&holder, state) {
                refusals.push(error);
                continue;
            }
            let Some(mounted) = self.mounted.get_mut(&holder) else {
                continue;
            };
            match mounted
                .app
                .apply_record(&record, &DocumentMutation::Snapshot(bytes.clone()))
            {
                Ok(()) => {
                    mounted.parts.insert(
                        route.clone(),
                        changes.iter().map(|(digest, _)| *digest).collect(),
                    );
                    self.beside.insert(route.clone(), holder.clone());
                    return Ok(holder);
                }
                Err(error) => refusals.push(error.to_string()),
            }
        }
        Err(format!(
            "no record of {} keeps {} beside it: {}",
            route.app_id,
            route.record,
            refusals.join("; ")
        ))
    }

    /// The record at `route` itself, mounted from what the workspace retains.
    fn mount_own(&mut self, route: &DocumentRoute, state: &WorkspaceState) -> Result<(), String> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if !self.mounted.contains_key(route) {
            let changes = state
                .records
                .get(route)
                .ok_or_else(|| format!("the workspace has no record {}", route.record))?;
            while self.mounted.len() >= MAX_MOUNTED {
                let Some(oldest) = self
                    .mounted
                    .iter()
                    .min_by_key(|(_, mounted)| mounted.used)
                    .map(|(route, _)| route.clone())
                else {
                    break;
                };
                self.mounted.remove(&oldest);
                self.beside.retain(|_, holder| *holder != oldest);
            }
            let app = ConfinedReplica::open(
                &self.catalogue,
                &route.app_id,
                &route.cluster_id,
                &self.participant,
                &crate::ReplicaActor::Owner,
                Seed::Existing {
                    record: reads::record_id(&route.record)?,
                    changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                },
            )
            .map_err(|error| error.to_string())?;
            let app = self.lend(app, &route.cluster_id);
            self.mounted.insert(
                route.clone(),
                Mounted {
                    app,
                    seen: changes.iter().map(|(digest, _)| *digest).collect(),
                    parts: HashMap::new(),
                    used: clock,
                },
            );
        }
        let mounted = self
            .mounted
            .get_mut(route)
            .ok_or_else(|| "the record did not mount".to_string())?;
        mounted.used = clock;
        Ok(())
    }

    /// Records of the workspace an app of the catalogue opens: those on the
    /// shared desktop first, then the most recently changed.
    /// The actions `route` lists: someone served within bounds is listed
    /// what their own replica offers them, what the host runs for them and
    /// nothing else; the owner, what the mounted record offers.
    fn listed(
        &mut self,
        workspace_id: u64,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, String> {
        match bounded {
            Some(who) => self
                .bounded_actions(workspace_id, who, route, state)
                .map_err(|error| error.to_string()),
            None => self
                .mount(route, state)
                .map(|mounted| live_actions(&mounted.app)),
        }
    }

    fn records(&self, state: &WorkspaceState) -> Vec<DocumentRoute> {
        let on_desktop = &state.on_desktop;
        let mut routes: Vec<DocumentRoute> = state
            .records
            .keys()
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .cloned()
            .collect();
        routes.sort_by_key(|route| {
            (
                !on_desktop.contains(&route.record),
                std::cmp::Reverse(state.changed.get(route).copied().unwrap_or(0)),
            )
        });
        routes
    }

    fn view_catalogue_page(
        &self,
        workspace: u64,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        let refuse = || ActionHostError::UnknownAction;
        let (wanted, provider) = exact_view_query(query)?;
        let route_hint = record_of(provider, "").ok_or_else(refuse)?;
        let link = self.shared.link.get().cloned().ok_or_else(refuse)?;
        let first = link
            .view_read_policy(workspace, caller, &route_hint.app_id, &route_hint.record)
            .ok_or_else(refuse)?;
        let route = record_of(provider, &first.workspace).ok_or_else(refuse)?;
        let source = link
            .view_document(workspace, caller, &route)
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        let app_id = self
            .catalogue
            .get(&route.app_id)
            .ok_or_else(refuse)?
            .app_id();
        // The document is decoded and the app's resource published in a
        // confined worker, never here.
        let mut views = ConfinedViews::load(app_id, source.bytes)
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        let descriptors = views.views(wanted, &first).map_err(|_| refuse())?;
        let fresh = link
            .view_read_policy(workspace, caller, &route.app_id, &route.record)
            .ok_or_else(refuse)?;
        if fresh.workspace != first.workspace
            || fresh.standing != first.standing
            || fresh.sandbox != first.sandbox
        {
            return Err(refuse());
        }
        let current = views.views(wanted, &fresh).map_err(|_| refuse())?;
        let rows = descriptors
            .into_iter()
            .filter(|descriptor| current.contains(descriptor))
            .map(|descriptor| {
                CatalogueRow::for_action(provider.to_owned(), route.app_id.clone(), descriptor)
            })
            .collect();
        let mut page = CataloguePage::from_rows(source.revision, rows, query);
        if !page.rows.is_empty() {
            page.locator = link.view_locator(&fresh.workspace, provider);
        }
        Ok(page)
    }

    /// Whether `workspace` admits `caller`: the person whose device runs this
    /// service, or a member the workspace's governance lets in. Answered here,
    /// on the service's own thread, where asking the host blocks.
    fn admits_member(&self, workspace: u64, caller: &Caller) -> bool {
        if self.serves_as_owner(caller) {
            return true;
        }
        self.bounded(caller)
            .is_some_and(|who| self.admits_bounded(workspace, &who))
    }

    fn catalogue_page(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        // A stranger is told nothing: the workspace's own records are for the
        // people it admits, and for the person whose device runs this service.
        // A view lookup is not a listing and carries its own governed check.
        if query.view_resource.is_none() && !self.admits_member(workspace_id, caller) {
            return Ok(CataloguePage::from_rows("0".into(), Vec::new(), query));
        }
        if let Some(provider) = query.provider.as_deref()
            && let Some(page) =
                self.registered_read_catalogue(workspace_id, caller, provider, query)
        {
            return Ok(page);
        }
        // A record named by its provider waits for its own keys; a listing
        // waits for the workspace's once.
        let named = query
            .provider
            .as_deref()
            .and_then(|provider| record_of(provider, ""));
        let needs = named
            .as_ref()
            .map_or(Needs::Workspace, |route| Needs::Read {
                app_id: &route.app_id,
                record: &route.record,
            });
        let state = self.refresh(workspace_id, needs)?;
        let revision = state.revision.to_string();
        // A workspace nothing was written in yet is named by the provider
        // asked for, so its new records are offered.
        let asked = query.provider.as_deref().and_then(provider_workspace);
        let Some(cluster_id) = state.workspace.clone().or_else(|| {
            asked
                .filter(|named| room_workspace_id(named).ok() == Some(workspace_id))
                .map(str::to_owned)
        }) else {
            return Ok(CataloguePage::from_rows(revision, Vec::new(), query));
        };
        let routes = self.records(&state);
        let own_provider = workspace_provider(&cluster_id);
        let wanted: Vec<DocumentRoute> = match query.provider.as_deref() {
            Some(provider) if provider == own_provider => Vec::new(),
            Some(provider) => {
                let route = record_of(provider, &cluster_id)
                    .filter(|route| routes.contains(route))
                    .ok_or(ActionHostError::UnknownAction)?;
                vec![route]
            }
            None => routes.iter().take(DEFAULT_ROWS_RECORDS).cloned().collect(),
        };
        let mut rows = Vec::new();
        let bounded = self.bounded(caller);
        for route in &wanted {
            let provider = record_provider(route);
            let mut descriptors = match self.listed(workspace_id, bounded.as_ref(), route, &state) {
                Ok(descriptors) => descriptors,
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "workspace actions: a record did not mount");
                    continue;
                }
            };
            if let Some(link) = self.shared.link.get().cloned()
                && let Ok(mut read) = reads::CanonicalRead::prepare(
                    &self.catalogue,
                    link.as_ref(),
                    workspace_id,
                    caller,
                    route,
                )
                && read
                    .recheck(link.as_ref(), workspace_id, caller, route)
                    .is_ok()
            {
                descriptors.extend(read.operations());
            }
            rows.extend(descriptors.into_iter().map(|descriptor| {
                CatalogueRow::for_action(provider.clone(), route.app_id.clone(), descriptor)
            }));
        }
        if query
            .provider
            .as_deref()
            .is_none_or(|provider| provider == own_provider)
            && self.may_across(workspace_id, caller, ActionKind::Undoable)
        {
            rows.extend(self.open_rows(&cluster_id));
        }
        let mut page = CataloguePage::from_rows(revision, rows, query);
        if query.provider.is_none() {
            page.providers = routes
                .iter()
                .map(|route| CatalogueProvider {
                    provider: record_provider(route),
                    app_id: route.app_id.clone(),
                    workspace: cluster_id.clone(),
                    record_id: route.record.clone(),
                    browsable: true,
                })
                .chain(std::iter::once(CatalogueProvider {
                    provider: own_provider,
                    app_id: String::new(),
                    workspace: cluster_id.clone(),
                    record_id: String::new(),
                    browsable: false,
                }))
                .collect();
        }
        Ok(page)
    }

    fn registered_read_catalogue(
        &self,
        workspace: u64,
        caller: &Caller,
        provider: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage> {
        let mut route = record_of(provider, "")?;
        let link = self.shared.link.get()?;
        route.cluster_id = link
            .view_read_policy(workspace, caller, &route.app_id, &route.record)?
            .workspace;
        let mut read = reads::CanonicalRead::prepare(
            &self.catalogue,
            link.as_ref(),
            workspace,
            caller,
            &route,
        )
        .ok()?;
        // Document rows retain their existing workspace revision and invocation
        // path. Only an exact registered-read target uses this source revision.
        let target = query.target.as_ref()?;
        let descriptors = read.operations();
        if !descriptors.iter().any(|entry| &entry.address == target) {
            return None;
        }
        read.recheck(link.as_ref(), workspace, caller, &route)
            .ok()?;
        let rows = descriptors
            .into_iter()
            .map(|descriptor| CatalogueRow::for_action(provider, route.app_id.clone(), descriptor))
            .collect();
        Some(CataloguePage::from_rows(
            read.revision().to_owned(),
            rows,
            query,
        ))
    }

    fn open_rows(&self, workspace: &str) -> Vec<CatalogueRow> {
        self.catalogue
            .iter()
            .map(|definition| open_row(workspace, definition.app_id()))
            .collect()
    }

    fn invoke(&mut self, workspace_id: u64, invocation: InvocationId) -> InvocationReceipt {
        let (caller, request) = {
            let mut ledger = self.ledger();
            let Some(entry) = ledger.entries.get(&invocation) else {
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected {
                        error: ActionHostError::UnknownInvocation,
                    },
                };
            };
            if entry.update.is_terminal() {
                // Cancelled before it started.
                return InvocationState::new(invocation, entry.update.clone()).receipt();
            }
            let caller = entry.caller.clone();
            let request = entry.request.clone();
            ledger.running = Some(invocation);
            drop(ledger);
            (caller, request)
        };
        if let Some(receipt) = self.start_terminal_run(workspace_id, &caller, &request) {
            let mut ledger = self.ledger();
            ledger.running = None;
            if matches!(receipt.status, ReceiptStatus::Rejected { .. }) {
                ledger.forget(&invocation);
            }
            drop(ledger);
            return receipt;
        }
        if let Some(receipt) = self.stop_terminal_runs(&caller, &request) {
            let mut ledger = self.ledger();
            ledger.running = None;
            if matches!(receipt.status, ReceiptStatus::Rejected { .. }) {
                ledger.forget(&invocation);
            }
            drop(ledger);
            return receipt;
        }
        let outcome = self.perform(workspace_id, &caller, &request);
        if matches!(
            outcome,
            Err(Outcome::Rejected(ActionHostError::ConfirmationRequired))
        ) && let Some(receipt) = self.ask_person(workspace_id, &caller, &request)
        {
            self.ledger().running = None;
            return receipt;
        }
        let target = &request.target;
        match &outcome {
            Ok(_) => {
                tracing::info!(provider = %target.provider, resource = %target.address.resource,
                action = %target.address.action, "workspace actions: succeeded");
            }
            Err(Outcome::Failed(error)) => tracing::info!(provider = %target.provider,
                resource = %target.address.resource, action = %target.address.action,
                code = %error.code, message = %error.message, "workspace actions: failed"),
            Err(Outcome::Rejected(error)) => tracing::info!(provider = %target.provider,
                resource = %target.address.resource, action = %target.address.action,
                code = error.code(), %error, "workspace actions: refused"),
        }
        let mut ledger = self.ledger();
        ledger.running = None;
        let (update, left_at) = match outcome {
            Ok((output, left_at)) => (InvocationUpdate::Succeeded { invocation, output }, left_at),
            Err(Outcome::Failed(error)) => (InvocationUpdate::Failed { invocation, error }, None),
            // Answered accepted while the person was asked: it ends refused,
            // and stays to say so.
            Err(Outcome::Rejected(error))
                if ledger
                    .entries
                    .get(&invocation)
                    .is_some_and(|entry| entry.asked) =>
            {
                let error = ActionFailure {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                };
                (InvocationUpdate::Failed { invocation, error }, None)
            }
            Err(Outcome::Rejected(error)) => {
                ledger.forget(&invocation);
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                };
            }
        };
        if let Some(entry) = ledger.entries.get_mut(&invocation) {
            entry.update = update.clone();
            entry.left_at = left_at;
        }
        drop(ledger);
        InvocationState::new(invocation, update).receipt()
    }

    /// The appearance `request` may show: only a bot whose installation
    /// allows it, and only on a record's own actions.
    fn admit_appearance(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<tilo_actions::Appearance>, ActionHostError> {
        let Some(appearance) = &request.appearance else {
            return Ok(None);
        };
        let link = self.shared.link.get();
        let admitted = tilo_actions::appearance::admit(
            caller,
            appearance,
            |installation| link.is_some_and(|link| link.bot_personas(workspace_id, installation)),
            |hash| link.and_then(|link| link.content_blob(workspace_id, hash)),
        )?;
        if provider_workspace(&request.target.provider).is_some() {
            return Err(ActionHostError::Refused {
                reason: "an appearance is shown only on a record's own actions".into(),
            });
        }
        Ok(Some(admitted))
    }

    /// `target`, its empty resource naming the record's own: a caller that
    /// knows the app but not its code (the desktop asking a connector) names
    /// its actions so.
    fn with_record_resource(
        &self,
        target: &tilo_actions::remote::RemoteActionTarget,
        app_id: &str,
    ) -> tilo_actions::remote::RemoteActionTarget {
        let mut target = target.clone();
        if target.address.resource.is_empty()
            && let Some(definition) = self.catalogue.get(app_id)
        {
            definition
                .record_kind
                .as_str()
                .clone_into(&mut target.address.resource);
        }
        target
    }

    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    /// Run `request`: what it answered, and where it left its actor in the
    /// record it changed.
    fn perform(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let appearance = self
            .admit_appearance(workspace_id, caller, request)
            .map_err(Outcome::Rejected)?;
        if let Some(output) = self
            .try_registered_read(workspace_id, caller, request)
            .map_err(|error| *error)?
        {
            return Ok((output, None));
        }
        if let Some((named, app)) = application_provider(&request.target.provider) {
            return self
                .show_on_desk(workspace_id, caller, named, app, &request.target)
                .map(|output| (output, None));
        }
        // Acting on a record waits for that record's keys and no other's;
        // opening an app on a new record needs only the workspace.
        let named = record_of(&request.target.provider, "");
        let needs = named.as_ref().map_or(Needs::Joined, |route| Needs::Act {
            app_id: &route.app_id,
            record: &route.record,
        });
        let state = self
            .refresh(workspace_id, needs)
            .map_err(Outcome::Rejected)?;
        if request
            .revision
            .as_ref()
            .is_some_and(|revision| *revision != state.revision.to_string())
        {
            return Err(Outcome::Rejected(ActionHostError::StaleRevision));
        }
        let workspace = state.workspace.clone();
        let target = &request.target;
        let owner = self.serves_as_owner(caller);
        if let Some(named) = provider_workspace(&target.provider) {
            return self
                .open_asked(workspace_id, caller, named, target)
                .map(|output| (output, None));
        }
        let workspace = workspace.ok_or_else(unknown_action)?;
        let route = record_of(&target.provider, &workspace)
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .ok_or_else(unknown_action)?;
        let target = &self.with_record_resource(target, &route.app_id);
        let arguments = invoke_arguments(&request.argument);
        let mounted = self.mount(&route, &state).map_err(|_| unknown_action())?;
        let live = live_actions(&mounted.app);
        if target.address.action == ui::tilo_resource::share::SHARE_LINK {
            return self
                .share_link(workspace_id, caller, &route, &live, target, arguments)
                .map(|output| (output, None));
        }
        if !owner {
            let who = self.bounded(caller).ok_or_else(unknown_action)?;
            if appearance.is_some() {
                return Err(unknown_action());
            }
            return self.perform_bounded(
                workspace_id,
                caller,
                &who,
                (request, &route, &state),
                (target, arguments, &live),
            );
        }
        let descriptor = live
            .iter()
            .find(|descriptor| descriptor.address == target.address)
            .ok_or_else(unknown_action)?;
        let arguments = descriptor.bind_prompt_text(arguments);
        let binding = bounded::WitnessBinding::of(workspace_id, caller, request);
        let confirmed = confirms(
            &descriptor.contract.confirmation,
            request.confirmation.as_ref(),
            |token| self.shared.witnesses.take(token, &binding),
        );
        let holder = self.holder_of(&route);
        let mounted = self.mounted.get_mut(&holder).ok_or_else(unknown_action)?;
        let context = caller_context(request);
        let output = mounted
            .app
            .invoke_in(
                context.as_ref(),
                appearance.as_ref(),
                &target.address,
                &arguments,
                confirmed,
            )
            .map_err(|error| door_outcome(&error))?;
        // Where the action left its actor, for the door that shows it
        // working: the text it wrote, the item it acted on.
        let left_at = mounted.app.presence().map(|(anchor, head)| ActorPlace {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
        });
        let taken = take_all(&mounted.app, &route)
            .map_err(|error| commit_failed(error.to_string(), false))?;
        self.commit_taken(workspace_id, &route, taken)
            .map_err(|reason| commit_failed(reason, true))?;
        Ok((output, left_at))
    }

    /// Open an app on a new record of the workspace `named`, as its
    /// workspace's own action asks (`app.<app>#open`), in the workspace the
    /// session is in: for the host's own person as its owner, and for anyone
    /// else, a member or an agent of anyone's, within their standing there
    /// now.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn open_asked(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        named: &str,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<ActionOutput, Outcome> {
        if room_workspace_id(named).ok() != Some(workspace_id) {
            return Err(unknown_action());
        }
        let who = if self.serves_as_owner(caller) {
            None
        } else {
            Some(self.bounded(caller).ok_or_else(unknown_action)?)
        };
        let app = target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .filter(|_| target.address.action == APPLICATION_OPEN_ACTION)
            .filter(|app| self.catalogue.get(app).is_some())
            .ok_or_else(unknown_action)?
            .to_owned();
        self.open_new(workspace_id, named, &app, who.as_ref())
    }

    /// Show the record an application's `open` names (its own record when it names none) as a
    /// window on the workspace's shared desktop, opened by `caller`: what a device of the owner's
    /// that draws no desk asks for the record it works in (an agent's `tilo mcp`), so every member
    /// sees it open as they see a colleague's window, without anyone's view moving to it
    /// ([`crate::desk`]). A record a window already shows is left where the people put it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn show_on_desk(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        named: &str,
        app: &str,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<ActionOutput, Outcome> {
        let unknown = || Outcome::Rejected(ActionHostError::UnknownAction);
        let person = self.participant.clone();
        if !self.acts_for_owner(caller)
            || target.address.action != APPLICATION_OPEN_ACTION
            || room_workspace_id(named).ok() != Some(workspace_id)
        {
            return Err(unknown());
        }
        let title = self
            .catalogue
            .get(app)
            .map(|definition| definition.title().to_owned())
            .ok_or_else(unknown)?;
        let desk = crate::participant::desktop_route(named);
        let state = self
            .refresh(
                workspace_id,
                Needs::Act {
                    app_id: &desk.app_id,
                    record: &desk.record,
                },
            )
            .map_err(Outcome::Rejected)?;
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: named.to_owned(),
            record: application_provider_record(&target.provider)
                .unwrap_or_default()
                .to_owned(),
        };
        if !state.records.contains_key(&route) {
            return Err(Outcome::Rejected(ActionHostError::Refused {
                reason: format!("this workspace has no {app} record {}", route.record),
            }));
        }
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| commit_failed("the workspace was not joined".into(), true))?;
        let view = crate::desk::View {
            app_id: app,
            title: &title,
            record: &route.record,
            beside: None,
        };
        let Some(change) = crate::desk::place_view(workspace.desktop.doc(), &person, &view) else {
            return Ok(ActionOutput::default());
        };
        workspace.desktop.receive(change.clone());
        self.commit(workspace_id, &desk, &DocumentMutation::Incremental(change))
            .map_err(|reason| commit_failed(reason, true))?;
        Ok(ActionOutput::default())
    }

    /// Commit what the owner's action changed, each record on its own
    /// route: a part it started is held beside the record it acted on.
    /// The error is why a commit failed.
    fn commit_taken(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        taken: Vec<(Option<DocumentRoute>, DocumentMutation)>,
    ) -> Result<(), String> {
        let holder = self.holder_of(route);
        for (changed, mutation) in taken {
            let changed = changed.unwrap_or_else(|| holder.clone());
            if changed != holder {
                self.beside.insert(changed.clone(), holder.clone());
            }
            self.commit(workspace_id, &changed, &mutation)?;
        }
        Ok(())
    }

    /// Whether `caller` is the host's own person, whom the service acts for.
    fn serves_as_owner(&self, caller: &Caller) -> bool {
        matches!(caller.principal(), CallerPrincipal::Person(person) if *person == self.participant)
    }

    /// Whether `caller` is the host's own person or an agent one of their
    /// devices handed its session to: who may open records, show them on the
    /// shared desk and run commands in the workspace's shell.
    fn acts_for_owner(&self, caller: &Caller) -> bool {
        acts_for(caller, &self.participant)
    }

    /// Put `request`, which `caller` (served within bounds) asked for and
    /// which needs the person's confirmation, to that person on their own
    /// devices: answered accepted, it runs on their allowing it and ends
    /// refused otherwise ([`bounded`]). `None` when it cannot be asked: the
    /// caller gave a confirmation of its own, the person was asked already,
    /// or this host has nobody to ask.
    fn ask_person(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<InvocationReceipt> {
        let who = self.bounded(caller)?;
        if request.confirmation.is_some() {
            return None;
        }
        let invocation = request.invocation;
        let workspace = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.cluster_id(workspace_id))?;
        let question = bounded::question(caller, &workspace, request).ok()?;
        if self
            .ledger()
            .entries
            .get_mut(&invocation)
            .is_none_or(|entry| std::mem::replace(&mut entry.asked, true))
        {
            return None;
        }
        let Some(asked) = self
            .shared
            .link
            .get()
            .and_then(|link| link.ask_person(workspace_id, &who.person_caller, question))
        else {
            if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
                entry.asked = false;
            }
            return None;
        };
        let shared = Arc::clone(&self.shared);
        let asking = bounded::Asking {
            workspace_id,
            invocation,
            binding: bounded::WitnessBinding::of(workspace_id, caller, request),
        };
        asked.then(move |reply| shared.answered(asking, reply.as_ref()));
        Some(InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }).receipt())
    }

    fn try_registered_read(
        &self,
        workspace: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<ActionOutput>, Box<Outcome>> {
        let Some(hint) = record_of(&request.target.provider, "") else {
            return Ok(None);
        };
        let Some(link) = self.shared.link.get() else {
            return Ok(None);
        };
        let Some(policy) = link.view_read_policy(workspace, caller, &hint.app_id, &hint.record)
        else {
            return Ok(None);
        };
        let route = DocumentRoute {
            cluster_id: policy.workspace,
            ..hint
        };
        let Ok(mut read) = reads::CanonicalRead::prepare(
            &self.catalogue,
            link.as_ref(),
            workspace,
            caller,
            &route,
        ) else {
            // A missing canonical read source must not replace or grant the
            // ordinary document door. Unregistered effects stay refused there.
            return Ok(None);
        };
        if !read
            .operations()
            .iter()
            .any(|entry| entry.address == request.target.address)
        {
            return Ok(None);
        }
        if request
            .revision
            .as_deref()
            .is_some_and(|revision| revision != read.revision())
        {
            return Err(Box::new(Outcome::Rejected(ActionHostError::Refused {
                reason: "the committed source changed".into(),
            })));
        }
        let arguments = match &request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::Text(_) => {
                return Err(Box::new(Outcome::Rejected(
                    ActionHostError::InvalidRequest {
                        reason: "a registered read requires typed arguments".into(),
                    },
                )));
            }
        };
        read.invoke(
            link.as_ref(),
            workspace,
            caller,
            &route,
            &request.target.address,
            &arguments,
        )
        .map(Some)
        .map_err(|error| Box::new(door_outcome(&error)))
    }

    /// A new record of `app` in `workspace`, created by the host's own person
    /// as its owner, or by `who` on their standing there now: a genesis they
    /// may not write is never saved, as no change of theirs is.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn open_new(
        &mut self,
        workspace_id: u64,
        workspace: &str,
        app: &str,
        who: Option<&bounded::Bounded>,
    ) -> Result<ActionOutput, Outcome> {
        let refused = |reason: String| Outcome::Rejected(ActionHostError::Refused { reason });
        let (creator, actor, permissions) = match who {
            None => (self.participant.clone(), crate::ReplicaActor::Owner, None),
            Some(who) => {
                let policy = self
                    .policy_for(workspace_id, who, workspace, None)
                    .map_err(Outcome::Rejected)?;
                let permissions = bounded::bounded_permissions(&policy, who.agent.as_deref());
                if permissions.answer(&TreePath::root().app(app), ActionKind::Undoable)
                    != Answer::Allow
                {
                    return Err(Outcome::Rejected(ActionHostError::NotPermitted));
                }
                let actor = crate::ReplicaActor::Member {
                    standing: policy.standing,
                    sandbox: policy.sandbox,
                    unix_time: policy.unix_time,
                    agent: who.agent.clone(),
                    agent_name: who.agent_name.clone(),
                };
                (who.person.clone(), actor, Some(permissions))
            }
        };
        let hosted =
            ConfinedReplica::open(&self.catalogue, app, workspace, &creator, &actor, Seed::New)
                .map_err(|error| refused(error.to_string()))?;
        let hosted = self.lend(hosted, workspace);
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            record: hosted.wire_record().to_owned(),
        };
        let genesis = hosted
            .take_mutation()
            .map_err(|error| refused(error.to_string()))?;
        if let (Some(permissions), Some(mutation)) = (&permissions, &genesis) {
            let unwritable =
                bounded::refused_writes(permissions, (app, None), &[], mutation, false)
                    .map_err(|reason| commit_failed(reason, false))?;
            if !unwritable.is_empty() {
                return Err(Outcome::Rejected(ActionHostError::NotPermitted));
            }
        }
        // When the host device takes part in the workspace's channels, it opens a
        // new record's channel for the member and hands it the keys, so the
        // first change goes out sealed instead of waiting for keys.
        if let Some(keys) = self
            .shared
            .link
            .get()
            .and_then(|link| link.host_keys(&route, true))
        {
            let keys = keys.map_err(|reason| {
                Outcome::Rejected(ActionHostError::Unavailable {
                    reason: format!("the host holds no keys to a new {app} record: {reason}"),
                })
            })?;
            self.workspaces
                .get(&workspace_id)
                .ok_or_else(|| refused("the workspace was not joined".into()))?
                .peer
                .install_channel_keys(route.clone(), keys)
                .map_err(refused)?;
        }
        if let Some(mutation) = genesis {
            self.commit(workspace_id, &route, &mutation)
                .map_err(refused)?;
        }
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "provider": record_provider(&route),
                "record": route.record,
            })),
            resources: Vec::new(),
        })
    }

    /// Publish `mutation` as the member: sealed with the record's channel
    /// keys, or held until a participant admits the member to it.
    fn commit(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or("the workspace was not joined")?;
        workspace.peer.publish(route.clone(), mutation)?;
        let bytes = bytes_of(mutation);
        let digest = change_digest(&self.member.endpoint.host_cert.device_public_key, &bytes);
        workspace.log.push((route.clone(), (digest, bytes)));
        if let Some(mounted) = self.mounted.get_mut(route) {
            mounted.seen.insert(digest);
        } else if let Some(holder) = self.beside.get(route)
            && let Some(mounted) = self.mounted.get_mut(holder)
        {
            mounted
                .parts
                .entry(route.clone())
                .or_default()
                .insert(digest);
        }
        Ok(())
    }

    /// [`Self::commit`], done only once the host holds the change durably
    /// (the session's barrier): handing it to the session, possibly to wait
    /// for keys, is not a save.
    fn commit_durably(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        self.commit(workspace_id, route, mutation)?;
        let saved = self
            .workspaces
            .get(&workspace_id)
            .ok_or("the workspace was not joined")?
            .peer
            .barrier()?;
        match saved.recv_timeout(COMMIT_WAIT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(format!("the host did not save the change: {reason}")),
            Err(_) => Err("the host has not acknowledged saving the change".into()),
        }
    }
}

impl Worker {
    /// `share-link` on a record this host runs headless. An app's root mints
    /// it where the app runs; here no root runs, so the host mints the same
    /// link from the resource's views as `caller` may read them now (the
    /// governed lookup a link's page makes too), under the web entry the
    /// host announces.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn share_link(
        &self,
        workspace_id: u64,
        caller: &Caller,
        route: &DocumentRoute,
        live: &[tilo_actions::ActionDescriptor],
        target: &tilo_actions::remote::RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, Outcome> {
        let failed = |message: String| {
            Outcome::Failed(ActionFailure {
                code: "share_link".into(),
                message,
                details: None,
                retryable: false,
            })
        };
        let resource = &target.address.resource;
        // The record's own actions name the resource's kind; its views are
        // attached where the app renders, which the lookup below runs.

        let kind = live
            .iter()
            .find(|descriptor| descriptor.address.resource == *resource)
            .map(|descriptor| descriptor.resource_kind.clone())
            .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        let query = CatalogueQuery {
            provider: Some(record_provider(route)),
            view_resource: Some(tilo_actions::remote::ViewResourceQuery {
                id: resource.clone(),
                kind: kind.clone(),
            }),
            ..CatalogueQuery::default()
        };
        let page = self
            .view_catalogue_page(workspace_id, caller, &query)
            .map_err(Outcome::Rejected)?;
        let views = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .filter(|descriptor| descriptor.address.resource == *resource)
            .filter_map(|descriptor| {
                descriptor
                    .view
                    .as_ref()
                    .filter(|view| view.open_action == descriptor.address.action)
            })
            .map(|view| ui::tilo_resource::share::SharedView {
                id: view.id.clone(),
                primary: view.primary,
                range: view.range.is_some(),
                desktop: view.desktop,
            })
            .collect();
        let request = ui::tilo_resource::share::ShareRequest {
            resource: resource.clone(),
            kind,
            views,
            invite: false,
            args: arguments,
        };
        let mut locator = page
            .locator
            .ok_or_else(|| failed("This workspace's host serves no web page".into()))?;
        // A part of a chain opens with the record it is held beside: the
        // link's page mounts that record, whose window holds the part and
        // offers its items at the address asked for here.
        let opened = self.holder_of(route);
        locator.provider = record_provider(&opened);
        let url = ui::framework::host_link(&request, &opened.app_id, &opened.record, &locator)
            .map_err(failed)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({ "url": url })),
            resources: Vec::new(),
        })
    }
}

/// The arguments an invocation carries, as the door binds them.
/// What a caller may say of its own invocation, which History keeps: the
/// work it belongs to (an agent's run), what it is for, and the gesture it
/// was part of. Who invoked it is the host's to say, from the session that
/// carried it, and what it did the action's.
fn caller_context(request: &InvokeRequest) -> Option<tilo_actions::ActionContext> {
    let said = request.context.as_ref()?;
    Some(tilo_actions::ActionContext {
        group: said.group.clone(),
        purpose: said.purpose.clone(),
        gesture: said.gesture,
        ..tilo_actions::ActionContext::default()
    })
}

fn invoke_arguments(argument: &InvokeArgument) -> serde_json::Map<String, serde_json::Value> {
    match argument {
        InvokeArgument::None => serde_json::Map::new(),
        InvokeArgument::Text(text) => {
            let mut arguments = serde_json::Map::new();
            arguments.insert(PROMPTED_PARAM.into(), text.clone().into());
            arguments
        }
        InvokeArgument::Json(arguments) => arguments.clone(),
    }
}

/// Whether `given`, from the host's own person on their own device, confirms
/// an action asking `policy`: their own word, the phrase it names, or a
/// token naming their answer that the host issued for this very invocation
/// (`witnessed` spends it).
fn confirms(
    policy: &ConfirmationPolicy,
    given: Option<&Confirmation>,
    witnessed: impl FnOnce(&str) -> bool,
) -> bool {
    match (policy, given) {
        (_, Some(Confirmation::Witnessed(token))) => witnessed(token),
        (ConfirmationPolicy::Phrase { phrase }, Some(Confirmation::Phrase(given))) => {
            phrase == given
        }
        (ConfirmationPolicy::Phrase { .. }, _) | (_, None) => false,
        (_, Some(Confirmation::Confirmed | Confirmation::Phrase(_))) => true,
    }
}

fn exact_view_query(
    query: &CatalogueQuery,
) -> Result<(&tilo_actions::remote::ViewResourceQuery, &str), ActionHostError> {
    let wanted = query
        .view_resource
        .as_ref()
        .ok_or(ActionHostError::UnknownAction)?;
    let provider = query
        .provider
        .as_deref()
        .ok_or(ActionHostError::UnknownAction)?;
    if wanted.id.is_empty()
        || wanted.id.trim().is_empty()
        || wanted.id.chars().any(char::is_control)
        || wanted.id.len() > 2048
        || wanted.kind.is_empty()
        || wanted.kind.trim().is_empty()
        || wanted.kind.chars().any(char::is_control)
        || wanted.kind.len() > 128
        || provider.len() > 2048
        || provider.chars().any(char::is_control)
        || query.target.is_some()
        || !query.cursor.is_empty()
        || query.parent_resource.is_some()
    {
        return Err(ActionHostError::UnknownAction);
    }
    Ok((wanted, provider))
}

/// Whether `caller` speaks for someone a workspace may admit as a member: a
/// person themselves, or the agent a device of theirs handed its session to
/// (an agent's grant), which every decision then holds to what an agent of
/// theirs may do. The workspace's governance still decides whether it admits
/// them.
fn speaks_for_a_member(caller: &Caller) -> bool {
    match caller.principal() {
        CallerPrincipal::Person(_) => true,
        CallerPrincipal::Agent(_) => {
            caller.is_session_delegated() && caller.acting_for_person().is_some()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{GaveUp, Needs, sealed_for};
    use std::collections::{BTreeMap, HashSet};
    use tilo_peer_state::document_frame::DocumentRoute;
    use tilo_session_host::session::RouteStatus;

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
    const OTHER_WORKSPACE: &str = "0b7f1d2e-5a6c-4d3b-8e9f-a0b1c2d3e4f5";

    fn route(workspace: &str, app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: workspace.into(),
            record: record.into(),
        }
    }

    fn sealed(awaiting: usize) -> RouteStatus {
        RouteStatus {
            awaiting_keys: awaiting,
            ..RouteStatus::default()
        }
    }

    #[test]
    fn a_request_waits_only_for_the_keys_of_what_it_reads() {
        let workspace = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();
        let a = route(WORKSPACE, "so.tilo.sheets", "a");
        let b = route(WORKSPACE, "so.tilo.sheets", "b");
        let elsewhere = route(OTHER_WORKSPACE, "so.tilo.sheets", "a");
        let mut routes = BTreeMap::new();
        routes.insert(a, sealed(0));
        routes.insert(b.clone(), sealed(3));
        routes.insert(elsewhere, sealed(1));
        let act = |record| Needs::Act {
            app_id: "so.tilo.sheets",
            record,
        };
        let read = |record| Needs::Read {
            app_id: "so.tilo.sheets",
            record,
        };
        let fresh = GaveUp::default();

        assert!(
            sealed_for(&routes, workspace, act("a"), &fresh).is_empty(),
            "another record's missing keys never hold back this one"
        );
        assert!(sealed_for(&routes, workspace, read("a"), &fresh).is_empty());
        assert_eq!(
            sealed_for(&routes, workspace, act("b"), &fresh),
            std::slice::from_ref(&b)
        );
        assert_eq!(
            sealed_for(&routes, workspace, read("b"), &fresh),
            std::slice::from_ref(&b)
        );
        assert!(
            sealed_for(
                &routes,
                workspace,
                Needs::Act {
                    app_id: "so.tilo.slides",
                    record: "b"
                },
                &fresh
            )
            .is_empty(),
            "a record is named by its app and id together"
        );
        assert_eq!(
            sealed_for(&routes, workspace, Needs::Workspace, &fresh),
            std::slice::from_ref(&b),
            "the first listing waits for this workspace's sealed records only"
        );
        assert!(sealed_for(&routes, workspace, Needs::Joined, &fresh).is_empty());

        let waited = GaveUp {
            listed: true,
            records: HashSet::from([b.clone()]),
        };
        assert!(
            sealed_for(&routes, workspace, Needs::Workspace, &waited).is_empty(),
            "later listings wait for nothing"
        );
        assert!(
            sealed_for(&routes, workspace, read("b"), &waited).is_empty(),
            "a listing waits for the same missing keys once"
        );
        assert_eq!(
            sealed_for(&routes, workspace, act("b"), &waited),
            [b],
            "acting on a record always waits for its keys"
        );
    }
}
