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
mod keys;
mod ledger;
mod link;
mod opened;
mod publish;
mod reads;
mod run_context;
mod run_voice;
mod terminal;
pub use content::MAX_PUT_BYTES;
pub use link::{
    FeedbackReviewer, ViewDocument, ViewReadPolicy, WorkspaceMember, WorkspaceRecordsLink,
};
pub use terminal::{ShellRan, TERMINAL_APP, Watching, WorkspaceShell};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_session_client::{Cmd, Event, Events, Link, Outbox};

use crate::apps::{AppCatalogue, Seed};
use crate::host::CatalogueFactory;
use crate::participant::actions::{Outcome, door_outcome, open_row, record_of, record_provider};
use crate::worker::{ConfinedReplica, ConfinedViews};
use crate::workspace_records::WorkspaceRecords;
use collab_types::DocumentMutation;
use keys::{Keys, catalogue_keys};
use ledger::Ledger;
use run_context::{caller_context, said_run_title};
use tilo_actions::InteractionPhase;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionHostError, ActorPlace, Caller, CallerPrincipal, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueRow, Completer, Confirmation, InvocationReceipt, InvocationState,
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
use tilo_session_client::{MemoryPeerStore, PeerConfig};
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
    /// An agent's run connected to a workspace and has asked nothing yet:
    /// it is present there, waiting ([`run_voice`]).
    Arrived { workspace_id: u64, caller: Caller },
    /// A workspace requests wait to be joined to changed: those it joined
    /// run.
    Joined(u64),
    /// A workspace was deleted: the service lets go of all it holds of it.
    Forget(u64),
}

impl Job {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Feedback { .. } => "feedback",
            Self::Catalogue { .. } => "catalogue",
            Self::Invoke { .. } => "invoke",
            Self::Ran(_) => "ran",
            Self::Arrived { .. } => "arrived",
            Self::Content { .. } => "content",
            Self::ContentGet { .. } => "content-get",
            Self::Joined(_) => "joined",
            Self::Forget(_) => "forget",
        }
    }

    /// The workspace whose records the job reads or changes.
    const fn workspace(&self) -> Option<u64> {
        match self {
            Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. }
            | Self::Arrived { workspace_id, .. } => Some(*workspace_id),
            Self::Feedback { .. } | Self::Ran(_) | Self::Joined(_) | Self::Forget(_) => None,
        }
    }

    /// What the job must have before it reads its workspace: what it is about
    /// to read names what it waits for. An invocation is named here by its id
    /// alone: the worker reads its request from the ledger
    /// ([`Worker::keys`]).
    fn keys(&self) -> Keys {
        match self {
            Self::Catalogue { query, .. } => catalogue_keys(query),
            Self::Feedback { .. }
            | Self::Invoke { .. }
            | Self::Ran(_)
            | Self::Content { .. }
            | Self::ContentGet { .. }
            | Self::Arrived { .. }
            | Self::Joined(_)
            | Self::Forget(_) => Keys::Joined,
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

    /// The workspace `workspace_id` was deleted: the service lets go of all
    /// it holds of it, its app workers' replicas included.
    pub fn forget_workspace(&self, workspace_id: u64) {
        if let Some(jobs) = self.shared.jobs.lock_recover().as_ref() {
            let _ = jobs.send((Instant::now(), Job::Forget(workspace_id)));
        }
    }

    /// `caller`'s session became an agent's run in `workspace_id`: the run is
    /// present there from now on, waiting, before its first request, as the
    /// host speaks for the runs it executes ([`run_voice`]). Nothing for a
    /// caller in no run.
    pub fn run_arrived(&self, workspace_id: u64, caller: &Caller) {
        if caller.agent_run().is_none() {
            return;
        }
        let _ = self.submit(Job::Arrived {
            workspace_id,
            caller: caller.clone(),
        });
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
        // Showing a record on the workspace's desk, as an agent's work opens
        // what it changed in front of the people there.
        let shows = application_provider(&request.target.provider).is_some()
            && request.target.address.action == APPLICATION_OPEN_ACTION;
        let member_request = (on_record || on_workspace || shows) && speaks_for_a_member(caller);
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
        match Workspace::join(member, workspace_id, &self.shared) {
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
            | RemoteActionMessage::RecordRead { .. }
            | RemoteActionMessage::RecordReading { .. }
            | RemoteActionMessage::RetiredRecordRequest
            | RemoteActionMessage::RetiredRecordResponse
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::Confirm { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
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
            if !ledger.insert(caller, request) {
                return receipt(ReceiptStatus::Rejected {
                    error: ActionHostError::Unavailable {
                        reason: "the host is running as many actions as it keeps track of; \
                                 ask again shortly"
                            .into(),
                    },
                });
            }
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
            stop.store(true, Ordering::SeqCst);
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
            | RemoteActionMessage::RecordRead { .. }
            | RemoteActionMessage::RecordReading { .. }
            | RemoteActionMessage::RetiredRecordRequest
            | RemoteActionMessage::RetiredRecordResponse
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::Confirm { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
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
    /// The revision of its record merged into it last.
    merged: Option<[u8; 32]>,
    /// The records held beside it (the parts of a chain its app keeps), by
    /// route, with the revision of each merged last.
    parts: HashMap<DocumentRoute, Option<[u8; 32]>>,
    used: u64,
}

/// Whether the wire record `record` is a part of a chain an app keeps
/// beside its own record ([`ui::record::RecordId::part`]): held with that
/// record, never on its own.
fn is_part(record: &str) -> bool {
    ui::record::RecordId::parse(record).is_some_and(|id| id.chain_part().is_some())
}

/// A record saved, and the revision it was saved at.
type Saved = (Vec<u8>, [u8; 32]);

/// A workspace as the session's replica holds it when a request is served.
struct WorkspaceState {
    workspace: Option<String>,
    /// Its records, those open on its shared desktop first, then the most
    /// recently changed.
    records: Vec<DocumentRoute>,
    engine: Arc<tilo_session_client::engine::Engine>,
    /// How many writes landed in it.
    revision: u64,
}

impl WorkspaceState {
    fn holds(&self, route: &DocumentRoute) -> bool {
        self.records.contains(route)
    }

    /// The record at `route`, saved, with its revision, unless it is still at
    /// `known`.
    fn document_since(
        &self,
        route: &DocumentRoute,
        known: Option<[u8; 32]>,
    ) -> Result<Option<Saved>, String> {
        self.engine
            .record_since(&route.cluster_id, &route.app_id, &route.record, known)
    }

    /// The record at `route`, saved, with its revision.
    fn document(&self, route: &DocumentRoute) -> Result<Saved, String> {
        self.document_since(route, None)?
            .ok_or_else(|| format!("the workspace has no record {} open here", route.record))
    }
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
    routes: &BTreeSet<DocumentRoute>,
    workspace_id: u64,
    needs: Needs<'_>,
    gave_up: &GaveUp,
) -> Vec<DocumentRoute> {
    // A part of a chain is held beside a record of its app that keeps it:
    // reading or acting on it needs that record open too, and any other
    // part of the same chain, which the request's host sends beside it.
    let named = |route: &DocumentRoute, app_id: &str, record: &str| {
        let same_chain = match (opened::chain_of(record), opened::chain_of(&route.record)) {
            (Some(asked), Some(held)) => asked == held,
            _ => false,
        };
        route.app_id == app_id
            && (route.record == record
                || same_chain
                || (is_part(record) && !is_part(&route.record)))
    };
    routes
        .iter()
        .filter(|route| {
            room_workspace_id(&route.cluster_id).ok() == Some(workspace_id)
                && match needs {
                    Needs::Joined => false,
                    Needs::Workspace => !gave_up.listed,
                    Needs::Read { app_id, record } => {
                        named(route, app_id, record) && !gave_up.records.contains(*route)
                    }
                    Needs::Act { app_id, record } => named(route, app_id, record),
                }
        })
        .cloned()
        .collect()
}

/// The service's membership of one workspace.
struct Workspace {
    peer: Link,
    events: Events,
    /// Set while requests wait for this workspace: its next change hands the
    /// service's thread [`Job::Joined`], which looks at what waited again.
    awaited: Arc<AtomicBool>,
    ready: bool,
    /// The workspace sends its records only when a device opens them (its
    /// `Ready`): what this member does not hold is a record it may still
    /// be sent, never one the workspace lacks.
    when_opened: bool,
    /// The records this member asked the workspace's host for and that it
    /// has not answered yet ([`Event::RecordOpened`]).
    asked: HashSet<DocumentRoute>,
    /// The records the host answered it holds none of.
    absent: HashSet<DocumentRoute>,
    /// What earlier waits for keys gave up on.
    gave_up: GaveUp,
    /// Records whose writes landed in the session's replica that it cannot open yet: their
    /// keys, surface or deciding governance have not arrived.
    undecided: BTreeSet<DocumentRoute>,
    /// The workspace's records in the session's replica, once a write to one
    /// named the workspace.
    records: Option<WorkspaceRecords>,
}

impl Workspace {
    fn join(
        member: &WorkspaceMember,
        workspace_id: u64,
        shared: &Arc<Shared>,
    ) -> Result<Self, String> {
        let _runtime = member.runtime.enter();
        let peer = tilo_session_client::start_session(
            PeerConfig {
                host: member.endpoint.host.clone(),
                cert: member.endpoint.host_cert.clone(),
                host_cert: member.endpoint.host_cert.clone(),
                revocations: RevocationList::new(),
                entering: None,
            },
            Arc::new(member.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        let awaited = Arc::new(AtomicBool::new(false));
        let (waiting, service) = (Arc::clone(&awaited), Arc::downgrade(shared));
        peer.on_change(Arc::new(move || {
            // Only the change that arrives while a request waits is worth the
            // thread's turn: the workspace's other changes still reach it as
            // documents.
            if waiting.swap(false, Ordering::SeqCst)
                && let Some(shared) = service.upgrade()
                && let Some(jobs) = shared.jobs.lock_recover().as_ref()
            {
                let _ = jobs.send((Instant::now(), Job::Joined(workspace_id)));
            }
        }));
        peer.send(Cmd::Join(workspace_id))?;
        Ok(Self {
            peer,
            events,
            awaited,
            ready: false,
            when_opened: false,
            asked: HashSet::new(),
            absent: HashSet::new(),
            gave_up: GaveUp::default(),
            undecided: BTreeSet::new(),
            records: None,
        })
    }

    fn pump(&mut self, workspace_id: u64) {
        for event in self.events.drain() {
            match event {
                Event::HostReady {
                    ws: ready,
                    records_when_opened,
                    ..
                } if ready == workspace_id => {
                    self.ready = true;
                    self.when_opened = records_when_opened;
                }
                Event::RecordOpened {
                    ws: opened,
                    route: Some(route),
                    frames,
                    ..
                } if opened == workspace_id => {
                    tracing::debug!(
                        app = %route.app_id,
                        record = %route.record,
                        frames,
                        asked = self.asked.contains(&route),
                        "workspace actions: the workspace answered for a record"
                    );
                    // The host sent all it holds of a record this member
                    // asked for: none means it holds none of it.
                    if self.asked.remove(&route) && frames == 0 {
                        self.absent.insert(route);
                    }
                }
                Event::Landed { route, .. } => {
                    if room_workspace_id(&route.cluster_id).ok() != Some(workspace_id) {
                        continue;
                    }
                    let engine = self.peer.engine();
                    // Undecided: the replica holds the record and cannot read it yet. Keys that
                    // arrived for a record it holds nothing of leave nothing undecided.
                    let undecided =
                        match engine.record(&route.cluster_id, &route.app_id, &route.record) {
                            Ok(read) => read.is_none(),
                            Err(_) => engine.records(&route.cluster_id).is_ok_and(|records| {
                                records.iter().any(|(app, record)| {
                                    *app == route.app_id && *record == route.record
                                })
                            }),
                        };
                    if undecided {
                        self.undecided.insert(route.clone());
                    } else {
                        self.undecided.remove(&route);
                    }
                    self.records
                        .get_or_insert_with(|| {
                            WorkspaceRecords::new(&route.cluster_id, Arc::clone(self.peer.engine()))
                        })
                        .landed(&route);
                }
                _ => {}
            }
        }
    }

    /// The workspace as the session's replica holds it now.
    fn state(&mut self) -> WorkspaceState {
        let engine = Arc::clone(self.peer.engine());
        match &mut self.records {
            Some(records) => WorkspaceState {
                workspace: Some(records.workspace().to_owned()),
                records: records.people_first(),
                engine,
                revision: records.landings(),
            },
            None => WorkspaceState {
                workspace: None,
                records: Vec::new(),
                engine,
                revision: 0,
            },
        }
    }

    /// The records of the workspace that `needs` reads and that hold
    /// undecided documents: they open once what the documents wait for
    /// arrives.
    fn sealed(&self, workspace_id: u64, needs: Needs<'_>) -> Vec<DocumentRoute> {
        sealed_for(&self.undecided, workspace_id, needs, &self.gave_up)
    }

    /// Take the keys of what `needs` reads in the joined workspace from the
    /// host's records, and say what the workspace opened. A request reaches
    /// here once its workspace is joined and what it reads opened, or once it
    /// waited [`JOIN_WAIT`] or [`KEYS_WAIT`] for that in vain
    /// ([`Worker::admit`]): nothing waits here, so one workspace's keys do not
    /// hold the service's thread against the other workspaces' requests.
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
        self.pump(workspace_id);
        if !self.ready {
            return Err(ActionHostError::Unavailable {
                reason: "the host has not joined the workspace yet".into(),
            });
        }
        // Keys the host device holds are handed over as soon as a route needs
        // them: the record acted on at once (fresh every time), and every
        // route that arrived sealed while the request waited.
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
        let sealed: Vec<DocumentRoute> = self
            .sealed(workspace_id, needs)
            .into_iter()
            .filter(|route| !handed.contains(route))
            .collect();
        if let Some(link) = link
            && !sealed.is_empty()
        {
            self.take_host_keys(sealed, needs, link, &mut handed)?;
        }
        let sealed = self.sealed(workspace_id, needs);
        match needs {
            Needs::Act { app_id, record } if !sealed.is_empty() => {
                // A record that travels with no id is the app's own record of
                // the workspace ([`ui::record::RecordId::package_primary`]);
                // naming it so says which record the caller waits on, rather
                // than leaving the name blank.
                let record = if record.is_empty() {
                    format!("{app_id}'s own record")
                } else {
                    format!("{app_id} record {record}")
                };
                Err(ActionHostError::Unavailable {
                    reason: format!("the host does not hold the keys to {record} yet"),
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
                        .send(Cmd::InstallChannelKeys(route, keys))
                        .map_err(|closed| ActionHostError::Unavailable {
                            reason: closed.to_string(),
                        })?;
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
    /// seen of it: its records, or, in a workspace with none yet, its governance.
    fn cluster_id(&self, workspace_id: u64) -> Option<String> {
        if let Some(records) = &self.records {
            return Some(records.workspace().to_owned());
        }
        self.peer
            .engine()
            .workspaces()
            .into_iter()
            .map(|workspace| workspace.to_string())
            .find(|workspace| room_workspace_id(workspace).ok() == Some(workspace_id))
    }
}

/// A request held off the service's thread until its workspace is ready for
/// it: joined, and holding the keys of what the request reads.
struct Waiting {
    /// When it arrived: the wait it reports, and what its deadline is counted
    /// from.
    queued: Instant,
    /// When it runs in any case, and says how far it got.
    deadline: Instant,
    job: Job,
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
    /// The routes whose channel this worker opened for the session, so a
    /// record the host starts is sealed from its first change
    /// ([`publish::Worker::commit`](publish)).
    channels_opened: HashSet<DocumentRoute>,
    /// Requests waiting for their workspace to be ready for them, oldest
    /// first, by workspace: the thread answers others meanwhile.
    joining: HashMap<u64, VecDeque<Waiting>>,
    clock: u64,
    /// What the host says for the runs it executes ([`run_voice`]).
    voices: run_voice::RunVoices,
    /// What the apps call the records the host changes, read in a confined
    /// worker kept for the next ([`publish`]).
    titles: crate::worker::ConfinedTitles,
}

/// The door's answer for an action it has nothing by that address.
fn unknown_action() -> Outcome {
    Outcome::Rejected(ActionHostError::UnknownAction)
}

/// Whether the page a request names was listed before `current` moved on.
fn stale_revision(revision: Option<&str>, current: &str) -> bool {
    revision.is_some_and(|revision| revision != current)
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
/// changed since it merged them last.
fn catch_up(route: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    match state.document_since(route, mounted.merged) {
        Ok(Some((bytes, revision))) => {
            if let Err(error) = mounted.app.apply(&DocumentMutation::Snapshot(bytes)) {
                tracing::warn!(%error, "workspace actions: a change did not merge");
            }
            mounted.merged = Some(revision);
        }
        Ok(None) => {}
        Err(error) => tracing::debug!(%error, "workspace actions: a record does not read"),
    }
    for (part, merged) in &mut mounted.parts {
        let Ok(record) = reads::record_id(&part.record) else {
            continue;
        };
        let Ok(Some((bytes, revision))) = state.document_since(part, *merged) else {
            continue;
        };
        if let Err(error) = mounted
            .app
            .apply_record(&record, &DocumentMutation::Snapshot(bytes))
        {
            tracing::warn!(%error, "workspace actions: a change to a part did not merge");
        }
        *merged = Some(revision);
    }
}

/// What a held record publishes that its door runs now.
fn live_actions(app: &ConfinedReplica) -> Vec<tilo_actions::ActionDescriptor> {
    app.survey()
}

/// Whether a failure is this invocation's answer. One the caller may ask
/// again is not: the same request under the same id runs it again rather
/// than being handed the failure it already has, which is what a change the
/// workspace took none of because the session it rode was replaced needs.
fn settles(error: &ActionFailure) -> bool {
    !error.retryable
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

/// Everything `app` changed since the last take, each with the route it
/// belongs to: the held record's own is `holder`'s, then each record held
/// beside it on the route of the same app and workspace its id names.
fn take_all(
    app: &ConfinedReplica,
    holder: &DocumentRoute,
) -> Result<Vec<(DocumentRoute, DocumentMutation)>, crate::apps::AppError> {
    let mut taken: Vec<(DocumentRoute, DocumentMutation)> = app
        .take_mutation()?
        .into_iter()
        .map(|mutation| (holder.clone(), mutation))
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
                (route, mutation)
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
            channels_opened: HashSet::new(),
            voices: run_voice::RunVoices::default(),
            titles: crate::worker::ConfinedTitles::default(),
            joining: HashMap::new(),
            clock: 0,
        }
    }

    fn run(mut self, jobs: &mpsc::Receiver<(Instant, Job)>) {
        loop {
            let due = match (self.joining_until(), self.voices.next_due()) {
                (Some(joining), Some(voice)) => Some(joining.min(voice)),
                (joining, voice) => joining.or(voice),
            };
            let next = match due {
                Some(until) => jobs.recv_timeout(until.saturating_duration_since(Instant::now())),
                None => jobs
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match next {
                Ok((queued, job)) => {
                    self.admit(queued, job);
                    // Requests that keep coming never hold back a run's word:
                    // the page forgets a member it has not heard from.
                    if self
                        .voices
                        .next_due()
                        .is_some_and(|due| due <= Instant::now())
                    {
                        self.tick_voices();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.give_up_joining();
                    self.tick_voices();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Run `job`, or have it wait off this thread until its workspace is ready
    /// for it: joined, and holding the keys of what it reads. A fresh
    /// workspace's first requests wait for its join and keys while the thread
    /// answers requests of workspaces already open, instead of every request
    /// waiting for every join and every key before it. A workspace's requests
    /// run in the order they came.
    fn admit(&mut self, queued: Instant, job: Job) {
        if let Some(workspace_id) = job.workspace() {
            let keys = self.keys(&job);
            // The workspace is in hand before the change is asked for below,
            // so a change that lands in between still wakes this thread.
            drop(self.membership(workspace_id));
            if !self.ready(workspace_id, keys.needs()) {
                self.hold(workspace_id, queued, job);
                return;
            }
            self.stop_waiting_if_idle(workspace_id);
        }
        self.timed(queued, job);
    }

    /// What `job` waits for before it can read its workspace.
    fn keys(&self, job: &Job) -> Keys {
        match job {
            Job::Invoke { invocation, .. } => self
                .ledger()
                .entries
                .get(invocation)
                .map_or(Keys::Joined, |entry| self.invoke_keys(&entry.request)),
            other => other.keys(),
        }
    }

    /// Hold `job` off the thread until `workspace_id`'s next change, and ask
    /// the session to hand this thread that change. Past [`JOIN_WAIT`] for a
    /// join and [`KEYS_WAIT`] for keys, the job runs anyway and says how far
    /// it got: a join that never comes, or keys that never arrive, answer the
    /// request rather than holding it for ever.
    fn hold(&mut self, workspace_id: u64, queued: Instant, job: Job) {
        let bound = if self.joined(workspace_id) {
            KEYS_WAIT
        } else {
            JOIN_WAIT
        };
        let deadline = queued.checked_add(bound).unwrap_or(queued);
        self.awaiting(workspace_id, true);
        self.joining
            .entry(workspace_id)
            .or_default()
            .push_back(Waiting {
                queued,
                deadline,
                job,
            });
    }

    /// Run the requests waiting for `workspace_id`, its change just arrived:
    /// those that are ready run here, and those still waiting for keys are
    /// held again with what they have waited so far.
    fn resume(&mut self, workspace_id: u64) {
        let Some(waiting) = self.joining.remove(&workspace_id) else {
            return;
        };
        self.awaiting(workspace_id, true);
        for Waiting {
            queued,
            deadline,
            job,
        } in waiting
        {
            let keys = self.keys(&job);
            if job.workspace() == Some(workspace_id) && !self.ready(workspace_id, keys.needs()) {
                self.joining
                    .entry(workspace_id)
                    .or_default()
                    .push_back(Waiting {
                        queued,
                        deadline,
                        job,
                    });
                continue;
            }
            self.timed(queued, job);
        }
        self.stop_waiting_if_idle(workspace_id);
    }

    /// Whether a request for `workspace_id` can run now: the workspace is
    /// joined, or it could not be joined at all, which the request reports;
    /// and what it reads has opened, or the host holds no keys to hand it.
    fn ready(&mut self, workspace_id: u64, needs: Needs<'_>) -> bool {
        if !self.joined(workspace_id) {
            return false;
        }
        matches!(needs, Needs::Joined) || self.opened(workspace_id, needs)
    }

    /// Hand this device the host's keys for the records of `workspace_id`
    /// `needs` reads that are still sealed, and say whether they opened. A
    /// record keeps waiting until what the keys open arrives: the session's
    /// next change wakes this thread to look again.
    fn opened(&mut self, workspace_id: u64, needs: Needs<'_>) -> bool {
        if self.membership(workspace_id).is_err() {
            // Nothing is there to read: the request says so itself.
            return true;
        }
        let Some(link) = self.shared.link.get().cloned() else {
            return true;
        };
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return true;
        };
        workspace.pump(workspace_id);
        // A record the workspace did not replay is asked for here, and the
        // host's answer is the change that wakes this thread to look again.
        if workspace.ask(workspace_id, needs) {
            return false;
        }
        let sealed = workspace.sealed(workspace_id, needs);
        if sealed.is_empty() {
            return true;
        }
        let mut handed = HashSet::new();
        let handed_now = workspace
            .take_host_keys(sealed, needs, link.as_ref(), &mut handed)
            .is_ok();
        // What the host holds no keys to is the request's to report.
        handed_now && workspace.sealed(workspace_id, needs).is_empty()
    }

    /// Whether a request for `workspace_id` can run now: the workspace is
    /// joined, or it could not be joined at all, which the request reports.
    fn joined(&mut self, workspace_id: u64) -> bool {
        match self.membership(workspace_id) {
            Ok(workspace) => {
                workspace.pump(workspace_id);
                workspace.ready
            }
            Err(_) => true,
        }
    }

    /// Ask the session to hand this thread `workspace_id`'s next change, or
    /// stop asking once nothing waits for it.
    fn awaiting(&self, workspace_id: u64, waiting: bool) {
        if let Some(workspace) = self.workspaces.get(&workspace_id) {
            workspace.awaited.store(waiting, Ordering::SeqCst);
        }
    }

    /// Stop asking to be woken for `workspace_id` once nothing waits for it.
    fn stop_waiting_if_idle(&self, workspace_id: u64) {
        if !self.joining.contains_key(&workspace_id) {
            self.awaiting(workspace_id, false);
        }
    }

    /// When the request that waited longest runs in any case: a join gives up
    /// later than keys do, and each request's own deadline counts from when it
    /// arrived.
    fn joining_until(&self) -> Option<Instant> {
        self.joining
            .values()
            .filter_map(VecDeque::front)
            .map(|waiting| waiting.deadline)
            .min()
    }

    /// Say again what the runs this host speaks for owe by now
    /// ([`run_voice::RunVoices::tick`]), each on its workspace's session.
    fn tick_voices(&mut self) {
        let workspaces = &self.workspaces;
        self.voices
            .tick(|workspace_id| workspaces.get(&workspace_id).map(|held| &held.peer));
    }

    /// Run the requests that waited in vain for their workspace or its keys:
    /// each says how far it got.
    fn give_up_joining(&mut self) {
        let now = Instant::now();
        let mut due = Vec::new();
        self.joining.retain(|_, waiting| {
            while waiting
                .front()
                .is_some_and(|waiting| waiting.deadline <= now)
            {
                due.extend(waiting.pop_front());
            }
            !waiting.is_empty()
        });
        for Waiting { queued, job, .. } in due {
            self.timed(queued, job);
        }
    }

    /// Take `job`, queued at `queued`, logging one that waited or ran long.
    fn timed(&mut self, queued: Instant, job: Job) {
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
                    self.voice_catalogue(workspace_id, &caller, &query);
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
                Job::Arrived {
                    workspace_id,
                    caller,
                } => self.voice_arrived(workspace_id, &caller),
                Job::Joined(workspace_id) => self.resume(workspace_id),
                Job::Forget(workspace_id) => self.forget(workspace_id),
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

    /// Let go of everything held of `workspace_id`, deleted: its
    /// membership, the records mounted and the replicas callers acted
    /// through there, and its content store. Requests still waiting for it
    /// run, and are told it is not there.
    fn forget(&mut self, workspace_id: u64) {
        let of =
            |route: &DocumentRoute| room_workspace_id(&route.cluster_id).ok() == Some(workspace_id);
        self.workspaces.remove(&workspace_id);
        self.shared.joins.lock_recover().remove(&workspace_id);
        self.shared.stores.lock_recover().remove(&workspace_id);
        self.mounted.retain(|route, _| !of(route));
        self.bounded.retain(|(route, _, _), _| !of(route));
        self.beside.retain(|route, held| !of(route) && !of(held));
        self.channels_opened.retain(|route| !of(route));
        for Waiting { queued, job, .. } in self.joining.remove(&workspace_id).unwrap_or_default() {
            self.timed(queued, job);
        }
    }

    /// The member's view of `workspace_id`, joining the workspace first and
    /// holding the keys of what `needs` reads, and every change the mounted
    /// records have not merged yet.
    /// The service's membership of `workspace_id`: the one joined when its
    /// first request arrived, or joined now.
    fn membership(&mut self, workspace_id: u64) -> Result<&mut Workspace, ActionHostError> {
        if !self.workspaces.contains_key(&workspace_id) {
            let ahead = self
                .shared
                .joins
                .lock_recover()
                .insert(workspace_id, None)
                .flatten();
            let workspace = match ahead {
                Some(workspace) => workspace,
                None => Workspace::join(&self.member, workspace_id, &self.shared)
                    .map_err(|reason| ActionHostError::Unavailable { reason })?,
            };
            self.workspaces.insert(workspace_id, workspace);
        }
        self.workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "the workspace was not joined".into(),
            })
    }

    fn refresh(
        &mut self,
        workspace_id: u64,
        needs: Needs<'_>,
    ) -> Result<WorkspaceState, ActionHostError> {
        self.membership(workspace_id)?;
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
        let state = workspace.state();
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
        let (bytes, revision) = state.document(route)?;
        let record = reads::record_id(&route.record)?;
        let holders: Vec<DocumentRoute> = state
            .records
            .iter()
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
                    mounted.parts.insert(route.clone(), Some(revision));
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
            let (bytes, revision) = state.document(route)?;
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
                    changes: vec![bytes],
                },
            )
            .map_err(|error| error.to_string())?;
            let app = self.lend(app, &route.cluster_id);
            self.mounted.insert(
                route.clone(),
                Mounted {
                    app,
                    merged: Some(revision),
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

    /// The records of `cluster_id` a listing shows: those this member's
    /// replica holds, and those the workspace's host holds of an app the
    /// catalogue has.
    ///
    /// A public workspace sends a device its records only when it opens one,
    /// so the replica holds a handful of them: without the host's list, every
    /// app there is installed and none running, and what a member's agent
    /// names is not found. A record listed here that the replica has not been
    /// sent is asked for when a request names it.
    fn records(&self, state: &WorkspaceState, cluster_id: &str) -> Vec<DocumentRoute> {
        let mut routes: Vec<DocumentRoute> = state.records.clone();
        if let Some(held) = self
            .shared
            .link
            .get()
            .and_then(|link| link.records(cluster_id))
        {
            for (app_id, record) in held {
                let route = DocumentRoute {
                    app_id,
                    cluster_id: cluster_id.to_owned(),
                    record,
                };
                if !routes.contains(&route) {
                    routes.push(route);
                }
            }
        }
        routes
            .into_iter()
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .collect()
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
        // Where the record opens is the caller's to know once they may read
        // it, views or not: a record's root links to the record alone.
        page.locator = link.view_locator(&fresh.workspace, provider);
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
        // waits for the workspace's once ([`catalogue_keys`], which the
        // service's thread admitted this request by).
        let keys = catalogue_keys(query);
        let state = self.refresh(workspace_id, keys.read())?;
        let revision = state.revision.to_string();
        // A workspace nothing was written in yet is named by the provider
        // asked for, so its new records are offered; one this member's replica
        // holds nothing of yet — a public workspace sends its records when a
        // device opens them — is named by the session it joined.
        let asked = query.provider.as_deref().and_then(provider_workspace);
        let Some(cluster_id) = state
            .workspace
            .clone()
            .or_else(|| {
                asked
                    .filter(|named| room_workspace_id(named).ok() == Some(workspace_id))
                    .map(str::to_owned)
            })
            .or_else(|| {
                self.workspaces
                    .get(&workspace_id)
                    .and_then(|workspace| workspace.cluster_id(workspace_id))
            })
        else {
            return Ok(CataloguePage::from_rows(revision, Vec::new(), query));
        };
        let routes = self.records(&state, &cluster_id);
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
        self.voice(
            workspace_id,
            &caller,
            &request,
            (&InteractionPhase::Applying, None),
        );
        if let Some(receipt) = self.terminal(workspace_id, &caller, &request) {
            return receipt;
        }
        let outcome = self.perform(workspace_id, &caller, &request);
        let about = self
            .ledger()
            .entries
            .get_mut(&invocation)
            .and_then(|entry| entry.about.take());
        if matches!(
            outcome,
            Err(Outcome::Rejected(ActionHostError::ConfirmationRequired))
        ) && let Some(about) = about
            && let Some(receipt) = self.ask_person(workspace_id, &caller, &request, &about)
        {
            self.ledger().running = None;
            return receipt;
        }
        log_outcome(&request.target, &outcome);
        let left = outcome
            .as_ref()
            .ok()
            .and_then(|(_, left_at)| left_at.as_ref());
        self.voice(
            workspace_id,
            &caller,
            &request,
            (&run_voice::ended_phase(&outcome), left),
        );
        let mut ledger = self.ledger();
        ledger.running = None;
        let (update, left_at) = match outcome {
            Ok((output, left_at)) => (InvocationUpdate::Succeeded { invocation, output }, left_at),
            Err(Outcome::Failed(error)) if !settles(&error) => {
                // A failure the caller may ask again is not this
                // invocation's answer: forgotten, the same request under the
                // same id runs it again rather than being handed the failure
                // it already has.
                ledger.forget(&invocation);
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Failed { error },
                };
            }
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
        if let Some((named, app)) = application_provider(&request.target.provider) {
            return self
                .show_on_desk(workspace_id, caller, (named, app), request)
                .map(|output| (output, None));
        }
        // Acting on a record waits for that record's keys and no other's;
        // opening an app on a new record needs only the workspace, and on the
        // record it keeps per workspace, the host's answer for that record
        // ([`Self::invoke_keys`], which the service's thread admitted this
        // request by).
        let keys = self.invoke_keys(request);
        let state = self
            .refresh(workspace_id, keys.needs())
            .map_err(Outcome::Rejected)?;
        // A page names its source's revision: the workspace's, or a registered
        // read's committed source (checked where that source is read, below).
        let workspace = state.workspace.clone();
        let target = &request.target;
        let owner = self.serves_as_owner(caller);
        if let Some(named) = provider_workspace(&target.provider) {
            if stale_revision(request.revision.as_deref(), &state.revision.to_string()) {
                return Err(Outcome::Rejected(ActionHostError::StaleRevision));
            }
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
        // What the record publishes, its own door runs. What only its
        // committed source publishes — a canonical read of it — is asked of
        // that source, which reads the record and opens a worker for it: an
        // action the record publishes never pays for that.
        let holder = self.holder_of(&route);
        let published_here = self.publishes(&holder, &target.address);
        if !published_here
            && let Some(output) = self
                .try_registered_read(workspace_id, caller, request)
                .map_err(|error| *error)?
        {
            return Ok((output, None));
        }
        if stale_revision(request.revision.as_deref(), &state.revision.to_string()) {
            return Err(Outcome::Rejected(ActionHostError::StaleRevision));
        }
        let mounted = self.mount(&route, &state).map_err(|_| unknown_action())?;
        if target.address.action == ui::tilo_resource::share::SHARE_LINK {
            // Sharing asks the record for everything it lists, which is what
            // the link is chosen from.
            let live = live_actions(&mounted.app);
            let at = (&route, &state, live.as_slice());
            return self
                .share_link(workspace_id, caller, at, target, arguments)
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
                (target, arguments),
            );
        }
        // The one action asked for, not the whole listing: a record of ten
        // thousand items costs this what a record of ten does.
        let descriptor = mounted
            .app
            .descriptor(&target.address)
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
        let context = caller_context(caller, request);
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
        let left_at = mounted.app.presence().map(bounded::placed);
        let taken = take_all(&mounted.app, &holder)
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
            // Its records have no window to show: starting the app is opening it.
            .filter(|_| {
                target.address.action == APPLICATION_OPEN_ACTION
                    || target.address.action == APPLICATION_START_ACTION
            })
            .filter(|app| self.catalogue.get(app).is_some())
            .ok_or_else(unknown_action)?
            .to_owned();
        self.open_new(workspace_id, named, &app, who.as_ref())
    }

    /// Show the record an application's `open` names (its own record when it names none) as a
    /// window on the workspace's shared desktop: what the device an agent acts through asks for
    /// each record the agent makes or changes (`tilo mcp`, the front door), so every member sees
    /// it open as they see a colleague's window, without anyone's view moving to it
    /// ([`crate::desk`]). The window is the person's the caller acts for — the host's own, or a
    /// member of the workspace — and names the agent run that opened it. A record a window
    /// already shows is left where the people put it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn show_on_desk(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        (named, app): (&str, &str),
        request: &InvokeRequest,
    ) -> Result<ActionOutput, Outcome> {
        let unknown = || Outcome::Rejected(ActionHostError::UnknownAction);
        let target = &request.target;
        if target.address.action != APPLICATION_OPEN_ACTION
            || room_workspace_id(named).ok() != Some(workspace_id)
        {
            return Err(unknown());
        }
        let person = if self.acts_for_owner(caller) {
            self.participant.clone()
        } else {
            self.bounded(caller)
                .filter(|who| self.admits_bounded(workspace_id, who))
                .ok_or_else(unknown)?
                .person
        };
        let run = caller_context(caller, request)
            .and_then(|context| context.group)
            .filter(|group| {
                group.kind == tilo_actions::context::AGENT_RUN_GROUP
                    || group.kind == tilo_actions::context::TASK_RUN_GROUP
            })
            .map(|group| tilo_desktop_model::ownership::OpenedBy {
                run: group.id,
                // The agent, by its own name; the run, by the title it gave.
                label: caller.agent_run().map_or(group.label, |run| run.label),
                title: group.title.unwrap_or_default(),
            });
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
        if !state.holds(&route) && !self.held_or_owed(workspace_id, &route)? {
            return Err(Outcome::Rejected(ActionHostError::Refused {
                reason: format!("this workspace has no {app} record {}", route.record),
            }));
        }
        let name = self
            .shared
            .link
            .get()
            .and_then(|link| link.workspace_name(workspace_id));
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
        let held = workspace
            .records
            .as_ref()
            .and_then(WorkspaceRecords::desktop);
        let opener = crate::desk::Opener {
            person: &person,
            run,
        };
        let empty = automerge::AutoCommit::new();
        let Some(change) = crate::desk::place_view(
            held.as_ref().unwrap_or(&empty),
            (named, name.as_deref()),
            &opener,
            &view,
        ) else {
            return Ok(ActionOutput::default());
        };
        let mutation = crate::desk::desk_mutation(held.is_some(), change);
        self.commit(workspace_id, &desk, &mutation)
            .map_err(|reason| commit_failed(reason, true))?;
        Ok(ActionOutput::default())
    }

    /// Commit what the owner's action changed, each record on its own
    /// route: a part it started is held beside the record it acted on.
    /// Whether the host comes to hold each durably is logged
    /// ([`Self::watch_saved`]). The error is why a commit failed.
    fn commit_taken(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        taken: Vec<(DocumentRoute, DocumentMutation)>,
    ) -> Result<(), String> {
        let holder = self.holder_of(route);
        let mut written = Vec::with_capacity(taken.len());
        for (changed, mutation) in taken {
            if changed != holder {
                self.beside.insert(changed.clone(), holder.clone());
            }
            self.commit(workspace_id, &changed, &mutation)?;
            written.push(publish::Written::of(&changed, &mutation));
        }
        self.watch_saved(workspace_id, written);
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
                let permissions =
                    bounded::bounded_permissions(&policy, who).map_err(Outcome::Rejected)?;
                if permissions.answer(&TreePath::root().app(app), ActionKind::Undoable)
                    != Answer::Allow
                {
                    return Err(Outcome::Rejected(ActionHostError::NotPermitted));
                }
                let actor = bounded::member_actor(&policy, who).map_err(Outcome::Rejected)?;
                (who.person.clone(), actor, Some(permissions))
            }
        };
        // An app that keeps one record per workspace opens that record: the
        // one the workspace holds when it has it, founded only when its host
        // answered it holds none.
        if self
            .catalogue
            .get(app)
            .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_record)
        {
            let route = DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                record: String::new(),
            };
            if self.held_or_owed(workspace_id, &route)? {
                return Ok(ActionOutput {
                    value: Some(serde_json::json!({
                        "provider": record_provider(&route),
                        "record": route.record,
                    })),
                    resources: Vec::new(),
                });
            }
        }
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
                bounded::refused_writes(permissions, (app, None), None, mutation, (None, false))
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
                .send(Cmd::InstallChannelKeys(route.clone(), keys))
                .map_err(|closed| refused(closed.to_string()))?;
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
}

impl Worker {
    /// `share-link` on a record this host runs headless. An app's root mints
    /// it where the app runs; here no root runs, so the host mints the same
    /// link from the resource's views as `caller` (a person: an agent asks as
    /// the person it acts for) may read them now (the governed lookup a link's
    /// page makes too), under the web entry the host announces.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn share_link(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        (route, state, live): (
            &DocumentRoute,
            &WorkspaceState,
            &[tilo_actions::ActionDescriptor],
        ),
        target: &tilo_actions::remote::RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, Outcome> {
        let asker = self.link_asker(workspace_id, caller, (route, state), target)?;
        let caller = &asker;
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
        // attached where the app renders, which the lookup below runs. A
        // resource with none is a record's root: its link names the record.
        let kind = live
            .iter()
            .find(|descriptor| descriptor.address.resource == *resource)
            .map(|descriptor| descriptor.resource_kind.clone())
            .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        // The record itself, opened at its primary view.
        let opened = self.holder_of(route);
        let root = self
            .catalogue
            .get(&route.app_id)
            .is_some_and(|definition| definition.record_kind.as_str() == resource.as_str());
        let plain = ["view", "start", "end"]
            .iter()
            .all(|name| arguments.get(*name).is_none_or(serde_json::Value::is_null));
        if root && plain && opened == *route {
            let request = ui::tilo_resource::share::ShareRequest {
                resource: resource.clone(),
                kind,
                views: Vec::new(),
                invite: false,
                args: arguments,
            };
            return self.record_link(workspace_id, caller, route, &request);
        }
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
        locator.provider = record_provider(&opened);
        let url = ui::framework::host_link(&request, &opened.app_id, &opened.record, &locator)
            .map_err(failed)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({ "url": url })),
            resources: Vec::new(),
        })
    }
}

impl Worker {
    /// Who asks for a link on `caller`'s behalf: someone served within
    /// bounds asks as the person they are or act for, and only for a link
    /// their own replica offers them.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn link_asker(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        (route, state): (&DocumentRoute, &WorkspaceState),
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<Caller, Outcome> {
        let Some(who) = self.bounded(caller) else {
            return Ok(caller.clone());
        };
        let offered = self
            .bounded_actions(workspace_id, &who, route, state)
            .map_err(Outcome::Rejected)?;
        if offered
            .iter()
            .any(|descriptor| descriptor.address == target.address)
        {
            Ok(who.person_caller)
        } else {
            Err(Outcome::Rejected(ActionHostError::UnknownAction))
        }
    }

    /// The link to the record at `route` itself, opened at its primary view:
    /// none of its views is looked up, only that `caller` may read it now
    /// and where it opens.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn record_link(
        &self,
        workspace_id: u64,
        caller: &Caller,
        route: &DocumentRoute,
        request: &ui::tilo_resource::share::ShareRequest,
    ) -> Result<ActionOutput, Outcome> {
        let failed = |message: String| {
            Outcome::Failed(ActionFailure {
                code: "share_link".into(),
                message,
                details: None,
                retryable: false,
            })
        };
        let refused = || Outcome::Rejected(ActionHostError::UnknownAction);
        let link = self.shared.link.get().ok_or_else(refused)?;
        let policy = link
            .view_read_policy(workspace_id, caller, &route.app_id, &route.record)
            .ok_or_else(refused)?;
        let locator = link
            .view_locator(&policy.workspace, &record_provider(route))
            .ok_or_else(|| failed("This workspace's host serves no web page".into()))?;
        let url = ui::framework::host_link(request, &route.app_id, &route.record, &locator)
            .map_err(failed)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({ "url": url })),
            resources: Vec::new(),
        })
    }
}

/// The arguments an invocation carries, as the door binds them.
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
/// person themselves, the agent a device of theirs handed its session to (an
/// agent's grant), which every decision then holds to what an agent of
/// theirs may do, or an agent device they certified, held to what the
/// workspace grants that agent. The workspace's governance still decides
/// whether it admits them.
fn speaks_for_a_member(caller: &Caller) -> bool {
    match caller.principal() {
        CallerPrincipal::Person(_) => true,
        CallerPrincipal::Agent(_) if caller.is_session_delegated() => {
            caller.acting_for_person().is_some()
        }
        CallerPrincipal::Agent(_) => {
            caller.user_public_key().is_some() && caller.device_public_key().is_some()
        }
        _ => false,
    }
}

/// Log how the action `target` names ended: succeeded, failed or refused.
fn log_outcome(
    target: &tilo_actions::remote::RemoteActionTarget,
    outcome: &Result<(ActionOutput, Option<tilo_actions::remote::ActorPlace>), Outcome>,
) {
    match outcome {
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
}

#[cfg(test)]
mod tests;
