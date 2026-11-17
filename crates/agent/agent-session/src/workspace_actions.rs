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
//!   desktop, the most recently changed next, up to [`DEFAULT_DOCUMENTS`];
//!   naming a record's provider lists every action that record publishes.
//! - **Invoke.** The record is held as a replica of its resource in its own
//!   confined app worker ([`ConfinedReplica`]) from what the service opened
//!   of it, the operation runs there through the resource's own API
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
//! ([`bounded`]): in a replica of their own on their current access, an
//! agent's bounded as every agent's is; what that replica offers is both
//! what their listing shows and all the host runs for them; an action that
//! asks first is put to the person; and nothing is saved that their access
//! does not let them write. Exact local View discovery also serves currently
//! certified workspace readers, through the host's canonical Read policy;
//! that grants no document invocation authority.
//!
//! The service joins each workspace as the host's own device ([`HostDevice`], from
//! the [`WorkspaceDocumentsLink`] the host supplies): a member of the workspace's
//! documents like any other, admitted by the workspace's members. It reads
//! only records it holds keys for and writes nothing in the clear. When the
//! host device itself takes part in the workspace's documents (a workspace the host
//! governs), it cannot be admitted to them a second time; the host hands the
//! service the keys it holds instead
//! ([`WorkspaceDocumentsLink::host_keys`]), for each record a request reads or
//! acts on, fresh for every act.

mod arguments;
mod asked_in_turn;
mod bounded;
mod canonical;
mod catalogue;
mod catch_up;
mod change;
mod content;
mod feedback;
mod found;
mod found_files;
mod host_actions;
mod invocation_log;
mod invocations;
mod jobs;
mod keys;
mod layout_window;
mod link;
mod listing;
mod member;
mod notes_folder;
mod opened;
mod opened_file;
mod placed;
mod publish;
mod reads;
mod run_context;
mod session_voice;
mod share;
mod terminal;
mod tree_put;
pub(super) use arguments::invoke_arguments;
pub use content::MAX_PUT_BYTES;
pub use host_actions::HostActions;
pub use link::{
    AskedInTurn, AsksInConversation, FeedbackReviewer, HostDevice, ViewDocument, ViewReadPolicy,
    WorkspaceDocumentsLink,
};
pub use terminal::{ShellRan, TERMINAL_APP, Watching, WorkspaceShell};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::engine::ResourceReading;
use tilo_session_client::{Cmd, Event, Link, Outbox};

use crate::apps::{AppCatalogue, Seed};
use crate::host::CatalogueFactory;
use crate::member::actions::{Outcome, api_outcome, record_provider, resource_of};
use crate::worker::{ConfinedReplica, ConfinedViews};
use crate::workspace_documents::WorkspaceDocuments;
use catch_up::{catch_up, hold_parts, kept_untold, tell_kept};
use collab_types::DocumentMutation;
use invocation_log::InvocationLog;
use jobs::Jobs;
use keys::{Keys, catalogue_keys};
use publish::Unsaved;
use run_context::{caller_context, said_invocation_title};
use tilo_actions::InteractionPhase;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionCallError, ActorPlace, Caller, CataloguePage, CatalogueQuery, CatalogueRow, Completer,
    Confirmation, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionMessage, application_provider,
    application_provider_resource, pending, provider_workspace,
};
use tilo_actions::{
    ActionFailure, ActionOutput, ConfirmationPolicy, InvocationId, InvocationUpdate, MemberId,
};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use ui::permissions::policy::{ActionKind, Answer, TreePath};

/// Records whose rows a catalogue naming no provider lists.
pub const DEFAULT_DOCUMENTS: usize = 12;
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
        workspace_id: WorkspaceId,
        query: Box<tilo_actions::remote::feedback::FeedbackQuery>,
        caller: Caller,
        done: Completer<Result<tilo_actions::remote::feedback::FeedbackAnswer, ActionCallError>>,
    },
    Catalogue {
        workspace_id: WorkspaceId,
        query: Box<CatalogueQuery>,
        caller: Caller,
        done: Completer<Result<CataloguePage, ActionCallError>>,
    },
    Invoke {
        workspace_id: WorkspaceId,
        invocation: InvocationId,
        done: Completer<InvocationReceipt>,
    },
    /// A Terminal session's run that ended in the host's shell, to record.
    Ran(Box<terminal::Ended>),
    /// A step of an upload into a workspace whose store the service has not
    /// found yet.
    Content {
        workspace_id: WorkspaceId,
        caller: Caller,
        step: tilo_actions::remote::ContentPutStep,
        done: Completer<Result<tilo_actions::remote::ContentPutAnswer, ActionCallError>>,
    },
    /// A piece of content asked of a workspace whose store the service has
    /// not found yet.
    ContentGet {
        workspace_id: WorkspaceId,
        caller: Caller,
        content: String,
        offset: u64,
        length: u64,
        done: Completer<Result<tilo_actions::remote::ContentGetAnswer, ActionCallError>>,
    },
    /// A link to content of a workspace, for a caller who may read it.
    ContentLink {
        workspace_id: WorkspaceId,
        caller: Caller,
        link: tilo_actions::remote::ContentLinkAsk,
        done: Completer<Result<tilo_actions::remote::ContentLink, ActionCallError>>,
    },
    /// A search of what a workspace holds.
    Search {
        workspace_id: WorkspaceId,
        caller: Caller,
        needle: String,
        within: tilo_actions::remote::Within,
        done: Completer<Result<Vec<tilo_actions::remote::Found>, ActionCallError>>,
    },
    /// The changes to a workspace's records after a cursor.
    Changes {
        workspace_id: WorkspaceId,
        caller: Caller,
        after: Option<String>,
        done: Completer<Result<tilo_actions::remote::ChangePage, ActionCallError>>,
    },
    /// A caller waits for what it follows in a workspace to change
    /// ([`tilo_actions::remote::ActionProvider::changed`]): a write landing in the workspace's
    /// records, or who is in it moving.
    Changed {
        workspace_id: WorkspaceId,
        caller: Caller,
        done: Completer<Result<(), ActionCallError>>,
    },
    /// A page of a workspace's runs ([`runs`]).
    Invocations {
        workspace_id: WorkspaceId,
        caller: Caller,
        query: tilo_actions::remote::workspace_invocations::InvocationsQuery,
        done: Completer<
            Result<tilo_actions::remote::workspace_invocations::Invocations, ActionCallError>,
        >,
    },
    /// An agent's run connected to a workspace and has asked nothing yet:
    /// it is present there, waiting ([`session_voice`]).
    Arrived {
        workspace_id: WorkspaceId,
        caller: Caller,
    },
    /// A workspace requests wait to be joined to changed: those it joined
    /// run.
    Joined(WorkspaceId),
    /// A workspace was deleted: the service lets go of all it holds of it.
    Forget(WorkspaceId),
    /// A workspace's notebook or folder took a write: its notes and their files in the
    /// folder are brought into step ([`notes_folder`]).
    NotesFolder(WorkspaceId),
    /// A record written since its name was last read: its name in its app's index is
    /// read again ([`publish`]).
    Rename(WorkspaceId, DocumentRoute),
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
            Self::ContentLink { .. } => "content-link",
            Self::Search { .. } => "search",
            Self::Changes { .. } => "changes",
            Self::Invocations { .. } => "invocations",
            Self::Changed { .. } => "changed",
            Self::Joined(_) => "joined",
            Self::Forget(_) => "forget",
            Self::NotesFolder(_) => "notes-folder",
            Self::Rename(..) => "rename",
        }
    }

    /// The workspace whose records the job reads or changes.
    const fn workspace(&self) -> Option<WorkspaceId> {
        match self {
            Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. }
            | Self::ContentLink { workspace_id, .. }
            | Self::Search { workspace_id, .. }
            | Self::Changes { workspace_id, .. }
            | Self::Invocations { workspace_id, .. }
            | Self::Changed { workspace_id, .. }
            | Self::Arrived { workspace_id, .. }
            | Self::NotesFolder(workspace_id)
            | Self::Rename(workspace_id, _) => Some(*workspace_id),
            Self::Feedback { .. } | Self::Ran(_) | Self::Joined(_) | Self::Forget(_) => None,
        }
    }

    /// What the job must have before it reads its workspace: what it is about
    /// to read names what it waits for. An invocation is named here by its id
    /// alone: the worker reads its request from the log
    /// ([`Worker::keys`]).
    fn keys(&self) -> Keys {
        match self {
            Self::Catalogue { query, .. } => catalogue_keys(query),
            // Every record the workspace holds is searched and followed.
            Self::Search { .. }
            | Self::Changes { .. }
            | Self::Invocations { .. }
            | Self::Changed { .. } => Keys::Workspace,
            Self::Feedback { .. }
            | Self::Invoke { .. }
            | Self::Ran(_)
            | Self::Content { .. }
            | Self::ContentGet { .. }
            | Self::ContentLink { .. }
            | Self::Arrived { .. }
            | Self::Joined(_)
            | Self::Forget(_)
            | Self::NotesFolder(_)
            | Self::Rename(..) => Keys::Joined,
        }
    }
}

/// A request that waited for the worker, or ran, at least this long is
/// logged: every request of a workspace waits behind the one before it, and
/// behind the requests of the other workspaces its thread serves.
const SLOW_JOB: Duration = Duration::from_secs(2);

struct Shared {
    log: Mutex<InvocationLog>,
    link: OnceLock<Arc<dyn WorkspaceDocumentsLink>>,
    jobs: Mutex<Option<Jobs>>,
    /// How the service joins workspaces, once its thread runs.
    host_device: OnceLock<HostDevice>,
    /// The host device's one session, in every workspace the service joined.
    member: OnceLock<Arc<member::Member>>,
    /// Every workspace a request named, by id: joined as the request
    /// arrives, while the service's one thread still answers others, so the
    /// first requests of many fresh workspaces join side by side instead of
    /// each waiting for the joins before it. `Some` until the thread takes
    /// the membership over.
    joins: Mutex<HashMap<WorkspaceId, Option<Workspace>>>,
    /// Each workspace's content store, once found.
    stores: content::Stores,
    /// The uploads callers have open.
    puts: tilo_actions::remote::HostedContentPuts,
    /// The answers people gave to invocations run for them.
    approvals: bounded::Approvals,
    /// What asks an agent's person in the conversation its turn is in.
    in_conversation: std::sync::RwLock<Option<Arc<dyn AsksInConversation>>>,
    /// Workspaces whose notes and folder wait for a pass to bring them into step.
    projecting: Mutex<HashSet<WorkspaceId>>,
    /// Records whose name in their app's index waits to be read again.
    renaming: Mutex<HashSet<DocumentRoute>>,
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
                log: Mutex::new(InvocationLog::default()),
                link: OnceLock::new(),
                jobs: Mutex::new(None),
                host_device: OnceLock::new(),
                member: OnceLock::new(),
                joins: Mutex::default(),
                stores: Mutex::default(),
                puts: tilo_actions::remote::HostedContentPuts::default(),
                approvals: bounded::Approvals::default(),
                in_conversation: std::sync::RwLock::default(),
                projecting: Mutex::default(),
                renaming: Mutex::default(),
            }),
            catalogue,
        }
    }

    /// Start serving `link`'s workspaces on the service's threads, one for
    /// each worker of the app worker pool ([`crate::worker::pool_size`]),
    /// and start the pool's workers. A second call is ignored.
    pub fn attach(&self, link: Arc<dyn WorkspaceDocumentsLink>) -> Result<(), String> {
        if self.shared.link.set(Arc::clone(&link)).is_err() {
            return Ok(());
        }
        crate::worker::prestart();
        let jobs = Jobs::start(&self.shared, &self.catalogue, &link)?;
        // Each thread holds its own handle on the link.
        drop(link);
        *self.shared.jobs.lock_recover() = Some(jobs);
        Ok(())
    }

    /// The workspace `workspace_id` was deleted: the service lets go of all
    /// it holds of it, its app workers' replicas included.
    pub fn forget_workspace(&self, workspace_id: WorkspaceId) {
        if let Some(jobs) = self.shared.jobs.lock_recover().as_ref() {
            let _ = jobs.send((Instant::now(), Job::Forget(workspace_id)));
        }
    }

    /// `caller`'s session became an agent's run in `workspace_id`: the run is
    /// present there from now on, waiting, before its first request, as the
    /// host speaks for the runs it executes ([`session_voice`]). Nothing for a
    /// caller in no run.
    pub fn session_arrived(&self, workspace_id: WorkspaceId, caller: &Caller) {
        if caller.agent_session().is_none() {
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
    /// their own current access, and refuses what it does not allow.
    fn admits(&self, caller: &Caller, request: &InvokeRequest) -> bool {
        let on_resource = resource_of(&request.target.provider, "").is_some();
        let link_request =
            request.target.address.action == ui::tilo_resource::share::SHARE_LINK && on_resource;
        let on_workspace = provider_workspace(&request.target.provider).is_some();
        // Showing a record on the workspace's layout, as an agent's work opens
        // what it changed in front of the people there.
        let shows = application_provider(&request.target.provider).is_some()
            && request.target.address.action == APPLICATION_OPEN_ACTION;
        let member_request = (on_resource || on_workspace || shows) && speaks_for_a_member(caller);
        link_request || member_request || self.authorized(caller)
    }

    /// Start joining `workspace_id` unless the service has: the thread
    /// waits only for what is still missing when it reaches the request.
    /// Joining sends the subscription and returns; the session runs on the
    /// member's runtime.
    fn join_ahead(&self, workspace_id: WorkspaceId) {
        let Some(host_device) = self.shared.host_device.get() else {
            return;
        };
        let mut joins = self.shared.joins.lock_recover();
        if joins.contains_key(&workspace_id) {
            return;
        }
        match Workspace::join(host_device, workspace_id, &self.shared) {
            Ok(workspace) => {
                joins.insert(workspace_id, Some(workspace));
                drop(joins);
            }
            Err(error) => {
                drop(joins);
                tracing::debug!(%error, workspace_id = %workspace_id, "workspace actions: a workspace was not joined ahead");
            }
        }
    }

    /// `workspace_id`'s notebook or folder took a write: bring its notes and their `.md`
    /// files in the folder into step ([`notes_folder`]). Writes that arrive while a pass
    /// waits to run are taken in by that pass.
    pub fn notes_folder_written(&self, workspace_id: WorkspaceId) {
        if self.shared.queue_notes_pass(workspace_id) {
            self.join_ahead(workspace_id);
        }
    }

    /// Whether `app`'s workspace record is the notebook whose notes are files in the
    /// workspace's folder: a workspace without one has no notes to bring into step.
    #[must_use]
    pub fn is_notebook(app: &str) -> bool {
        app == notes_folder::NOTES_APP
    }

    /// Whether a write to `app`'s record is one [`Self::notes_folder_written`] follows.
    #[must_use]
    pub fn follows_notes_folder(app: &str) -> bool {
        app == notes_folder::NOTES_APP || app == tilo_fs::tree_document::FILES_APP
    }

    /// Whether `caller` started invocation `id` here.
    #[must_use]
    pub fn owns(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.log().owned(caller, id).is_some()
    }

    /// How invocation `id` of `caller` stands here now, when this host holds it: what a caller
    /// follows a long-running action with to learn what it made, and what a host reads to place a
    /// window once an open's record exists.
    #[must_use]
    pub fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let update = {
            let log = self.log();
            log.entries
                .get(id)
                .filter(|entry| entry.caller == *caller)?
                .update
                .clone()
        };
        Some(InvocationState::new(*id, update))
    }

    fn log(&self) -> std::sync::MutexGuard<'_, InvocationLog> {
        self.shared.log.lock_recover()
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
    #[expect(
        clippy::too_many_lines,
        reason = "one answer for each message of the wire's vocabulary, in its order"
    )]
    pub fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let unavailable = || ActionCallError::Unavailable {
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
            // What an app this service mounts in its own process publishes:
            // its signed build names its surface, readable before the app's
            // first window, so a call through generated types is checked
            // against it as it is once a window is open.
            RemoteActionMessage::SurfaceRequest { correlation, app } => {
                // Mounting the catalogue names every app's committed surface;
                // asked before any app opened, the service mounts it now.
                let surface = ui::app_surface::bytes_of(&app).or_else(|| {
                    (self.catalogue)();
                    ui::app_surface::bytes_of(&app)
                });
                Pending::ready(RemoteActionMessage::SurfaceResponse {
                    correlation,
                    result: Ok(surface.map(|bytes| String::from_utf8_lossy(bytes).into_owned())),
                })
            }
            RemoteActionMessage::Observe { invocation, .. } => {
                let state = self.log().owned(caller, &invocation).map(|entry| {
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
            RemoteActionMessage::ContentLinkRequest { correlation, link } => {
                self.content_link(workspace_id, caller, correlation, link)
            }
            RemoteActionMessage::SearchRequest {
                correlation,
                needle,
                within,
            } => {
                let (done, answer) = pending();
                let job = Job::Search {
                    workspace_id,
                    caller: caller.clone(),
                    needle,
                    within,
                    done,
                };
                if let Err(Job::Search { done, .. }) = self.submit(job) {
                    done.complete(Err(unavailable()));
                }
                answer.map(move |result| RemoteActionMessage::SearchResponse {
                    correlation,
                    result,
                })
            }
            RemoteActionMessage::ChangesRequest { correlation, after } => {
                let (done, answer) = pending();
                let job = Job::Changes {
                    workspace_id,
                    caller: caller.clone(),
                    after,
                    done,
                };
                if let Err(Job::Changes { done, .. }) = self.submit(job) {
                    done.complete(Err(unavailable()));
                }
                answer.map(move |result| RemoteActionMessage::ChangesResponse {
                    correlation,
                    result,
                })
            }
            // What a call follows of the workspace is read again on either scope when a write
            // lands in its records, so the device need not tell them apart.
            RemoteActionMessage::ChangedRequest { correlation, .. } => {
                let (done, answer) = pending();
                let job = Job::Changed {
                    workspace_id,
                    caller: caller.clone(),
                    done,
                };
                if let Err(Job::Changed { done, .. }) = self.submit(job) {
                    done.complete(Err(unavailable()));
                }
                answer.map(move |result| RemoteActionMessage::ChangedResponse {
                    correlation,
                    result,
                })
            }
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::ResourceRead { .. }
            | RemoteActionMessage::ResourceReading { .. }
            | RemoteActionMessage::RetiredResourceRequest
            | RemoteActionMessage::RetiredResourceResponse
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::Confirm { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
            | RemoteActionMessage::RetiredResourceWatch { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::ContentLinkResponse { .. }
            | RemoteActionMessage::SearchResponse { .. }
            | RemoteActionMessage::ChangesResponse { .. }
            | RemoteActionMessage::ChangedResponse { .. }
            | RemoteActionMessage::SurfaceResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return None,
        })
    }

    /// Admit `request` from `caller` and run it on the service's thread,
    /// answering its receipt.
    fn invoke_request(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        // Answered at admission: a wait is ignored, as an older host
        // would, and is no part of the remembered request.
        request.wait_ms = None;
        let invocation = request.invocation;
        // The listing of the workspace's runs is a read, answered at once and
        // remembered as no run of its own.
        if tilo_actions::remote::workspace_invocations::is_listing(&request.target) {
            return self.invocations_request(workspace_id, caller, &request);
        }
        let receipt = |status| {
            Pending::ready(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status,
            }))
        };
        if !self.admits(caller, &request) {
            return receipt(ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction,
            });
        }
        {
            let mut log = self.log();
            if let Some(entry) = log.entries.get(&invocation) {
                if entry.caller != *caller || !entry.request.same_invocation(&request) {
                    return receipt(ReceiptStatus::Rejected {
                        error: ActionCallError::InvocationConflict,
                    });
                }
                let state = InvocationState::new(invocation, entry.update.clone());
                return receipt(state.receipt().status);
            }
            if !log.insert(caller, request) {
                return receipt(ReceiptStatus::Rejected {
                    error: ActionCallError::Unavailable {
                        reason: "the host is running as many actions as it keeps track of; \
                                 ask again shortly"
                            .into(),
                    },
                });
            }
            if let Some(entry) = log.entries.get_mut(&invocation) {
                entry.workspace = Some(workspace_id);
            }
        }
        let (done, answer) = pending();
        let job = Job::Invoke {
            workspace_id,
            invocation,
            done,
        };
        if let Err(Job::Invoke { done, .. }) = self.submit(job) {
            self.log().forget(&invocation);
            done.complete(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::Unavailable {
                        reason: "the host's workspace actions are not running".into(),
                    },
                },
            });
        }
        answer.map(RemoteActionMessage::Receipt)
    }

    fn feedback_request(
        &self,
        workspace_id: WorkspaceId,
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
            done.complete(Err(ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(move |result| RemoteActionMessage::FeedbackResponse {
            correlation,
            result,
        })
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionCallError> {
        let mut log = self.log();
        let running = log.running.contains(id);
        let entry = log
            .entries
            .get_mut(id)
            .filter(|entry| entry.caller == *caller)
            .ok_or(ActionCallError::UnknownInvocation)?;
        if entry.update.is_terminal() {
            return Err(ActionCallError::NotCancellable {
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
            return Err(ActionCallError::NotCancellable {
                reason: "the action is already running".into(),
            });
        }
        if entry.saving {
            return Err(ActionCallError::NotCancellable {
                reason: "the action ran; its change is being saved".into(),
            });
        }
        entry.update = InvocationUpdate::Cancelled {
            invocation: *id,
            message: Some("cancelled before it started".into()),
        };
        drop(log);
        Ok(())
    }
}

/// One record held on the service thread, in its own app worker.
struct Mounted {
    app: ConfinedReplica,
    /// The heads of its record merged into it last: what the next merge
    /// takes only the changes after.
    merged: Option<Vec<[u8; 32]>>,
    /// The records held beside it (the parts of a chain its app keeps), by
    /// route, with the heads of each merged last.
    parts: HashMap<DocumentRoute, Option<Vec<[u8; 32]>>>,
    /// Parts of the app's chains this record keeps none of (another
    /// record's chain): refused once, never handed to it again.
    foreign: std::collections::HashSet<DocumentRoute>,
    /// What it and its parts had merged when it was last asked which parts
    /// it wants ([`catch_up::hold_parts`]): asked again only once that moved.
    wanted_at: Option<u64>,
    used: u64,
}

/// Whether the wire record `record` is a part of a chain an app keeps
/// beside its own record ([`ui::resource::DocumentId::part`]): held with that
/// record, never on its own.
fn is_part(record: &str) -> bool {
    ui::resource::DocumentId::parse(record).is_some_and(|id| id.chain_part().is_some())
}

/// How a person reads the record `record` of `app` in a message or a log: an
/// app's own record of the workspace travels with no id
/// ([`ui::resource::DocumentId::package_primary`]), so it is named for its app
/// rather than left blank.
fn named_document(app: &str, record: &str) -> String {
    if record.is_empty() {
        format!("{app}'s own record")
    } else {
        format!("{app} record {record}")
    }
}

/// [`named_document`] of the record at `route`.
pub(super) fn named(route: &DocumentRoute) -> String {
    named_document(&route.app_id, &route.document)
}

/// What running an action came to: its answer and where it left its actor.
pub(super) type Performed = Result<(ActionOutput, Option<ActorPlace>), Outcome>;

/// A record saved, and the heads it was saved at, sorted.
pub(super) type Saved = (Vec<u8>, Vec<[u8; 32]>);

/// What a replica merges of a record, and the heads the record stands at
/// once it has.
type Update = (DocumentMutation, Vec<[u8; 32]>);

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

    /// What changed of the record at `route` since `held`, the heads a
    /// replica merged last: only the changes after them where the record
    /// holds them, the record whole where it does not (or where nothing was
    /// merged yet), and nothing when it still stands there. With the heads
    /// it stands at.
    fn update(
        &self,
        route: &DocumentRoute,
        held: Option<&[[u8; 32]]>,
    ) -> Result<Option<Update>, String> {
        Ok(
            match self.engine.record_update(
                &route.cluster_id,
                &route.app_id,
                &route.document,
                held,
            )? {
                ResourceReading::Captured { heads, bytes, .. } => {
                    Some((DocumentMutation::Snapshot(bytes), heads))
                }
                ResourceReading::Changes { heads, bytes, .. } => {
                    Some((DocumentMutation::Incremental(bytes), heads))
                }
                ResourceReading::Unchanged(_) | ResourceReading::Pending => None,
            },
        )
    }

    /// The record at `route`, saved, with its heads.
    fn document(&self, route: &DocumentRoute) -> Result<Saved, String> {
        match self
            .engine
            .record_update(&route.cluster_id, &route.app_id, &route.document, None)?
        {
            ResourceReading::Captured { heads, bytes, .. } => Ok((bytes, heads)),
            _ => Err(format!("the workspace holds no {} yet", named(route))),
        }
    }
}

/// What a request must have opened before it reads the workspace.
#[derive(Clone, Copy, Debug)]
pub(super) enum Needs<'a> {
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
    workspace_id: WorkspaceId,
    needs: Needs<'_>,
    gave_up: &GaveUp,
) -> Vec<DocumentRoute> {
    // A part of a chain is held beside a record of its app that keeps it:
    // reading or acting on it needs that record open too, and any other
    // part of the same chain, which the request's host sends beside it.
    let named = |route: &DocumentRoute, app_id: &str, record: &str| {
        let same_chain = match (opened::chain_of(record), opened::chain_of(&route.document)) {
            (Some(asked), Some(held)) => asked == held,
            _ => false,
        };
        route.app_id == app_id
            && (route.document == record
                || same_chain
                || (is_part(record) && !is_part(&route.document)))
    };
    routes
        .iter()
        .filter(|route| {
            WorkspaceId::parse(&route.cluster_id) == Some(workspace_id)
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

/// The service's membership of one workspace, on the host device's one
/// session ([`member`]); it leaves the workspace when dropped.
struct Workspace {
    id: WorkspaceId,
    peer: Link,
    member: Arc<member::Member>,
    ready: bool,
    /// The workspace sends its records only when a device opens them (its
    /// `Ready`): what this member does not hold is a record it may still
    /// be sent, never one the workspace lacks.
    when_opened: bool,
    /// The records this member asked the workspace's host for and that it
    /// has not answered yet ([`Event::DocumentOpened`]).
    asked: HashSet<DocumentRoute>,
    /// The records the host answered for, asked once: a chain's head arrives
    /// on the route of the part it names, never its own, so asking until it
    /// is held would ask without end.
    answered: HashSet<DocumentRoute>,
    /// Of those, the records the host answered it holds none of.
    absent: HashSet<DocumentRoute>,
    /// Records the host answered are another person's own: never sent here, and never founded.
    not_yours: HashSet<DocumentRoute>,
    /// What earlier waits for keys gave up on.
    gave_up: GaveUp,
    /// Records whose writes landed in the session's replica that it cannot open yet: their
    /// keys, surface or deciding governance have not arrived.
    undecided: BTreeSet<DocumentRoute>,
    /// The workspace's records in the session's replica, once a write to one
    /// named the workspace.
    records: Option<WorkspaceDocuments>,
    /// What the host says for the runs it executes here ([`session_voice`]).
    sessions: session_voice::Sessions,
    /// What changed in its records, for those following it ([`found`]).
    changes: ui::history_source::feed::ChangeLog,
    /// What its records' histories say of its runs ([`runs`]).
    run_index: invocations::Index,
    /// Who waits for what a watch of this workspace follows to change ([`change::Watchers`]).
    watchers: change::Watchers,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.member.leave(self.id);
    }
}

impl Workspace {
    fn join(
        host_device: &HostDevice,
        workspace_id: WorkspaceId,
        shared: &Arc<Shared>,
    ) -> Result<Self, String> {
        let member = Arc::clone(
            shared
                .member
                .get_or_init(|| member::Member::start(host_device, shared)),
        );
        member.join(workspace_id)?;
        Ok(Self {
            sessions: session_voice::Sessions::new(
                workspace_id,
                host_device.endpoint.host_cert.device_public_key,
            ),
            id: workspace_id,
            peer: member.peer().clone(),
            member,
            ready: false,
            when_opened: false,
            asked: HashSet::new(),
            answered: HashSet::new(),
            absent: HashSet::new(),
            not_yours: HashSet::new(),
            gave_up: GaveUp::default(),
            undecided: BTreeSet::new(),
            records: None,
            changes: ui::history_source::feed::ChangeLog::new(),
            run_index: invocations::Index::default(),
            watchers: change::Watchers::default(),
        })
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
    /// arrives. A workspace's folder is read record by record, so a request
    /// reading the tree waits for the chunks its own record names too.
    fn sealed(&self, workspace_id: WorkspaceId, needs: Needs<'_>) -> Vec<DocumentRoute> {
        let mut sealed: BTreeSet<DocumentRoute> =
            sealed_for(&self.undecided, workspace_id, needs, &self.gave_up)
                .into_iter()
                .collect();
        for route in self.routes(workspace_id, needs) {
            if self.undecided.contains(&route) && !self.gave_up.records.contains(&route) {
                sealed.insert(route);
            }
        }
        sealed.into_iter().collect()
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
        workspace_id: WorkspaceId,
        needs: Needs<'_>,
        link: Option<&dyn WorkspaceDocumentsLink>,
    ) -> Result<(), ActionCallError> {
        self.pump(workspace_id);
        if !self.ready {
            return Err(ActionCallError::Unavailable {
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
                document: record.to_owned(),
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
                let record = named_document(app_id, record);
                Err(ActionCallError::Unavailable {
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
    /// ([`WorkspaceDocumentsLink::host_keys`]), noting each in `handed`. The
    /// session then opens what it held for them and wakes the wait. A record
    /// to act on whose keys the host refuses is refused at once; a listing
    /// goes on without it.
    fn take_host_keys(
        &self,
        routes: Vec<DocumentRoute>,
        needs: Needs<'_>,
        link: &dyn WorkspaceDocumentsLink,
        handed: &mut HashSet<DocumentRoute>,
    ) -> Result<(), ActionCallError> {
        for route in routes {
            handed.insert(route.clone());
            match link.host_keys(&route, false) {
                None => {}
                Some(Ok(keys)) => {
                    self.peer
                        .send(Cmd::InstallGroupKeys(route, keys))
                        .map_err(|closed| ActionCallError::Unavailable {
                            reason: closed.to_string(),
                        })?;
                }
                Some(Err(reason)) => {
                    if let Needs::Act { app_id, record } = needs {
                        return Err(ActionCallError::Unavailable {
                            reason: format!(
                                "the host holds no keys to {app_id} record {record}: {reason}"
                            ),
                        });
                    }
                    tracing::info!(app = %route.app_id, record = %route.document, %reason,
                        "workspace actions: listing without a record the host holds no keys to");
                }
            }
        }
        Ok(())
    }

    /// The cluster id of the workspace `workspace_id` names, from what the member has
    /// seen of it: its records, or, in a workspace with none yet, its governance.
    fn cluster_id(&self, workspace_id: WorkspaceId) -> Option<String> {
        if let Some(records) = &self.records {
            return Some(records.workspace().to_owned());
        }
        self.peer
            .engine()
            .workspaces()
            .into_iter()
            .map(|workspace| workspace.to_string())
            .find(|workspace| WorkspaceId::parse(workspace) == Some(workspace_id))
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
    host_device: HostDevice,
    catalogue: AppCatalogue,
    shared: Arc<Shared>,
    member: ui::permissions::policy::PersonId,
    workspaces: HashMap<WorkspaceId, Workspace>,
    mounted: BTreeMap<DocumentRoute, Mounted>,
    /// The replicas callers served within bounds act through ([`bounded`]).
    bounded: BTreeMap<bounded::BoundedKey, bounded::BoundedReplica>,
    /// Callers' replicas whose last write is still on its way to be saved,
    /// kept for the caller's next action ([`bounded::Parked`]).
    parked: bounded::Parked<bounded::BoundedReplica>,
    /// The route of the mounted record each part is held beside.
    beside: HashMap<DocumentRoute, DocumentRoute>,
    /// The routes whose document this worker opened for the session, so a
    /// record the host starts is sealed from its first change
    /// ([`publish::Worker::commit`](publish)).
    channels_opened: HashSet<DocumentRoute>,
    /// Requests waiting for their workspace to be ready for them, oldest
    /// first, by workspace: the thread answers others meanwhile.
    joining: HashMap<WorkspaceId, VecDeque<Waiting>>,
    clock: u64,
    /// What the apps call the records the host changes, read in a confined
    /// worker kept for the next ([`publish`]).
    titles: crate::worker::ConfinedTitles,
    /// The writes the job being served handed to the session, by workspace:
    /// an invocation is answered done only once the host holds them
    /// ([`publish::Worker::saving`]).
    handed: HashMap<WorkspaceId, Vec<publish::Written>>,
    /// The registered reads listings offer, kept between them
    /// ([`canonical`]).
    canonical: canonical::CanonicalReads,
    /// How many passes over each workspace's notes and their files failed in a row
    /// ([`notes_folder`]): the next is tried again after a wait that grows with them.
    notes_failures: HashMap<WorkspaceId, u32>,
    /// The notebook and folder of the workspaces whose notes were projected last, as the
    /// last pass left them ([`notes_folder::Followed`]).
    notes_followed: HashMap<WorkspaceId, notes_folder::Followed>,
    /// When each record's name was last read for its app's index ([`publish`]).
    named: HashMap<DocumentRoute, Instant>,
    /// Why each document a listing offers did not mount, as it was told last
    /// ([`listing`]): an agent lists at every step it takes, so a cause is
    /// logged once, not once per listing.
    unmounted: HashMap<DocumentRoute, String>,
}

/// The API's answer for an action it has nothing by that address.
fn unknown_action() -> Outcome {
    Outcome::Rejected(ActionCallError::UnknownAction)
}

/// Whether the page a request names was listed before `current` moved on.
fn stale_revision(revision: Option<&str>, current: &str) -> bool {
    revision.is_some_and(|revision| revision != current)
}

/// Whether `caller` is `person`, or an agent a device of theirs handed its
/// session to.
fn acts_for(caller: &Caller, person: &str) -> bool {
    if let Some(id) = caller.acting().person_id() {
        return id == person;
    }
    caller.acting().is_agent()
        && caller
            .delegating_person()
            .is_some_and(|delegating| delegating.acting().person_id() == Some(person))
}

/// What a held record publishes that its API runs now.
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
pub(super) fn commit_failed(message: String, retryable: bool) -> Outcome {
    Outcome::Failed(ActionFailure {
        code: "commit_failed".into(),
        message,
        details: None,
        retryable,
    })
}

/// Everything `app` changed since the last take, each with the route it
/// belongs to: the held record's own is `holder`'s, then each record held
/// beside it on the route of the same app and workspace its id names. All
/// or none: when the records beside cannot be taken, the record's own change
/// is put back for the next take, never dropped with the failure.
fn take_all(
    app: &ConfinedReplica,
    holder: &DocumentRoute,
) -> Result<Vec<(DocumentRoute, DocumentMutation)>, crate::apps::AppError> {
    let own = app.take_mutation()?;
    let beside = match app.take_document_mutations() {
        Ok(beside) => beside,
        Err(error) => {
            if let Some(own) = own {
                app.put_back(own);
            }
            return Err(error);
        }
    };
    let mut taken: Vec<(DocumentRoute, DocumentMutation)> = own
        .into_iter()
        .map(|mutation| (holder.clone(), mutation))
        .collect();
    taken.extend(beside.into_iter().map(|(record, mutation)| {
        let route = DocumentRoute {
            app_id: holder.app_id.clone(),
            cluster_id: holder.cluster_id.clone(),
            document: record.route().to_owned(),
        };
        (route, mutation)
    }));
    Ok(taken)
}

/// The longest a pass over a workspace's notes waits for the requests queued
/// behind it on the worker's thread.
const PROJECTION_WAIT: Duration = Duration::from_secs(2);

impl Shared {
    /// Queue a pass over `workspace_id`'s notes and their files, unless one waits to run
    /// already, which takes in whatever this one would have: whether it was queued.
    fn queue_notes_pass(&self, workspace_id: WorkspaceId) -> bool {
        if !self.projecting.lock_recover().insert(workspace_id) {
            return false;
        }
        let sent = self.jobs.lock_recover().as_ref().is_some_and(|jobs| {
            jobs.send((Instant::now(), Job::NotesFolder(workspace_id)))
                .is_ok()
        });
        if !sent {
            self.projecting.lock_recover().remove(&workspace_id);
        }
        sent
    }
}

impl Worker {
    fn new(
        link: &dyn WorkspaceDocumentsLink,
        catalogue: AppCatalogue,
        shared: Arc<Shared>,
    ) -> Self {
        let host_device = link.host_device();
        let _ = shared.host_device.set(host_device.clone());
        Self {
            member: ui::permissions::policy::PersonId::from_root(link.user_public_key()),
            host_device,
            catalogue,
            shared,
            workspaces: HashMap::new(),
            mounted: BTreeMap::new(),
            bounded: BTreeMap::new(),
            parked: bounded::Parked::default(),
            beside: HashMap::new(),
            channels_opened: HashSet::new(),
            titles: crate::worker::ConfinedTitles::default(),
            joining: HashMap::new(),
            clock: 0,
            handed: HashMap::new(),
            canonical: canonical::CanonicalReads::default(),
            notes_failures: HashMap::new(),
            notes_followed: HashMap::new(),
            named: HashMap::new(),
            unmounted: HashMap::new(),
        }
    }

    fn run(mut self, jobs: &mpsc::Receiver<(Instant, Job)>) {
        // Passes over a workspace's notes and their files, waiting for the
        // requests queued behind them: a pass is what a person's folder shows,
        // never what a caller waits on, so it goes once no request waits.
        let mut projections: VecDeque<(Instant, Job)> = VecDeque::new();
        loop {
            if projections
                .front()
                .is_some_and(|(queued, _)| queued.elapsed() >= PROJECTION_WAIT)
            {
                // Requests that keep coming never keep a folder behind for long.
                if let Some((queued, job)) = projections.pop_front() {
                    self.admit(queued, job);
                }
                continue;
            }
            if !projections.is_empty() {
                match jobs.try_recv() {
                    Ok((queued, job)) if matches!(job, Job::NotesFolder(_)) => {
                        projections.push_back((queued, job));
                    }
                    Ok((queued, job)) => self.admit(queued, job),
                    Err(mpsc::TryRecvError::Empty) => {
                        if let Some((queued, job)) = projections.pop_front() {
                            self.admit(queued, job);
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
                continue;
            }
            let due = [
                self.joining_until(),
                session_voice::next_due(&self.workspaces),
                self.watchers_until(),
            ]
            .into_iter()
            .flatten()
            .min();
            let next = match due {
                Some(until) => jobs.recv_timeout(until.saturating_duration_since(Instant::now())),
                None => jobs
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            self.expire_watchers();
            match next {
                Ok((queued, job)) if matches!(job, Job::NotesFolder(_)) => {
                    projections.push_back((queued, job));
                }
                Ok((queued, job)) => {
                    self.admit(queued, job);
                    // Requests that keep coming never hold back a run's word:
                    // the page forgets a member it has not heard from.
                    if session_voice::next_due(&self.workspaces)
                        .is_some_and(|due| due <= Instant::now())
                    {
                        self.tick_voices();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.give_up_joining();
                    self.tick_voices();
                    self.expire_watchers();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Invocation `job`, or have it wait off this thread until its workspace is ready
    /// for it: joined, and holding the keys of what it reads. A fresh
    /// workspace's first requests wait for its join and keys while the thread
    /// answers requests of workspaces already open, instead of every request
    /// waiting for every join and every key before it. A workspace's requests
    /// run in the order they came.
    fn admit(&mut self, queued: Instant, job: Job) {
        // A workspace's change woke this thread to run what waited for it.
        // Nothing waits usually, and most wakes then cost nothing: reading
        // the workspace for each wake would make a busy workspace pay a read
        // for every change the service hears, including the changes of every
        // other workspace.
        if matches!(job, Job::Joined(_)) {
            self.timed(queued, job);
            return;
        }
        if let Some(workspace_id) = job.workspace() {
            let keys = self.keys(&job);
            // The workspace is in hand before the change is asked for below,
            // so a change that lands in between still wakes this thread.
            drop(self.membership(workspace_id));
            if !self.ready(workspace_id, &keys) {
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
                .log()
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
    fn hold(&mut self, workspace_id: WorkspaceId, queued: Instant, job: Job) {
        let bound = if self.joined(workspace_id) {
            KEYS_WAIT
        } else {
            JOIN_WAIT
        };
        let deadline = queued.checked_add(bound).unwrap_or(queued);
        self.joining
            .entry(workspace_id)
            .or_default()
            .push_back(Waiting {
                queued,
                deadline,
                job,
            });
        // A change that landed before the workspace was asked to wake this
        // thread woke nobody; `resume` looks again now that it is.
        self.resume(workspace_id);
    }

    /// Invocation the requests waiting for `workspace_id`, its change just arrived:
    /// those that are ready run here, and those still waiting for keys are
    /// held again with what they have waited so far.
    fn resume(&mut self, workspace_id: WorkspaceId) {
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
            if job.workspace() == Some(workspace_id) && !self.ready(workspace_id, &keys) {
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
    /// Nothing else holds a request back: an open waits for no window
    /// layout, which only shows what the action did.
    fn ready(&mut self, workspace_id: WorkspaceId, keys: &Keys) -> bool {
        if !self.joined(workspace_id) {
            return false;
        }
        let needs = keys.needs();
        matches!(needs, Needs::Joined) || self.opened(workspace_id, needs)
    }

    /// Hand this device the host's keys for the records of `workspace_id`
    /// `needs` reads that are still sealed, and say whether they opened. A
    /// record keeps waiting until what the keys open arrives: the session's
    /// next change wakes this thread to look again.
    fn opened(&mut self, workspace_id: WorkspaceId, needs: Needs<'_>) -> bool {
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
        // host's answer is the change that wakes this thread to look again. A
        // workspace's folder is read record by record: asking for the tree's
        // own record asks for the chunks it names too
        // ([`opened::Workspace::routes`]).
        if workspace.ask(workspace_id, needs) {
            return false;
        }
        // The chains its app keeps beside the document it names are asked for
        // too, and waited for.
        if self.ask_chains(workspace_id, needs) {
            return false;
        }
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return true;
        };
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
    fn joined(&mut self, workspace_id: WorkspaceId) -> bool {
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
    fn awaiting(&self, workspace_id: WorkspaceId, waiting: bool) {
        if let Some(workspace) = self.workspaces.get(&workspace_id) {
            workspace.member.await_change(workspace_id, waiting);
        }
    }

    /// Stop asking to be woken for `workspace_id` once nothing waits for it.
    fn stop_waiting_if_idle(&self, workspace_id: WorkspaceId) {
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
    /// ([`session_voice::Sessions::tick`]), each on its workspace's session.
    fn tick_voices(&mut self) {
        let now = tilo_session_client::presence::now_ms();
        for workspace in self.workspaces.values_mut() {
            workspace.sessions.tick(&workspace.peer, now);
        }
    }

    /// Invocation the requests that waited in vain for their workspace or its keys:
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

    /// Take `job`, queued at `queued`, logging every one at debug and one
    /// that waited or ran long at warn.
    fn timed(&mut self, queued: Instant, job: Job) {
        let started = Instant::now();
        let kind = job.kind();
        self.take(job);
        // What it ended, it ended now, as a listing of runs says.
        self.log().stamp_ended();
        let (waited, ran) = (started.duration_since(queued), started.elapsed());
        tracing::debug!(
            kind,
            waited_ms = waited.as_millis(),
            ran_ms = ran.as_millis(),
            "workspace actions: a request ran"
        );
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
                    None => done.complete(Err(ActionCallError::Unavailable {
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
                } => self.arrived(workspace_id, &caller),
                Job::Search {
                    workspace_id,
                    caller,
                    needle,
                    within,
                    done,
                } => done.complete(self.search(workspace_id, &caller, (&needle, &within))),
                Job::Changes {
                    workspace_id,
                    caller,
                    after,
                    done,
                } => done.complete(self.changes(workspace_id, &caller, after.as_deref())),
                Job::Changed {
                    workspace_id,
                    caller,
                    done,
                } => self.watch(workspace_id, &caller, done),
                Job::Invocations {
                    workspace_id,
                    caller,
                    query,
                    done,
                } => done.complete(self.invocations(workspace_id, &caller, &query)),
                Job::Joined(workspace_id) => self.resume(workspace_id),
                Job::NotesFolder(workspace_id) => self.notes_pass(workspace_id),
                Job::Rename(workspace_id, route) => self.rename(workspace_id, &route),
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
                Job::ContentLink {
                    workspace_id,
                    caller,
                    link,
                    done,
                } => done.complete(self.content_link(workspace_id, &caller, &link)),
            }
        }
        // What a job that answers no invocation wrote (a run it records), or
        // what a failed one wrote before it failed, is logged once saved.
        self.watch_handed();
    }

    fn log(&self) -> std::sync::MutexGuard<'_, InvocationLog> {
        self.shared.log.lock_recover()
    }

    /// Let go of everything held of `workspace_id`, deleted: its
    /// membership, the records mounted and the replicas callers acted
    /// through there, and its content store. Requests still waiting for it
    /// run, and are told it is not there.
    fn forget(&mut self, workspace_id: WorkspaceId) {
        let of =
            |route: &DocumentRoute| WorkspaceId::parse(&route.cluster_id) == Some(workspace_id);
        self.workspaces.remove(&workspace_id);
        self.shared.joins.lock_recover().remove(&workspace_id);
        self.shared.stores.lock_recover().remove(&workspace_id);
        self.mounted.retain(|route, _| !of(route));
        self.bounded.retain(|(route, _, _), _| !of(route));
        self.parked.retain(|(route, _, _)| !of(route));
        self.beside.retain(|route, held| !of(route) && !of(held));
        self.channels_opened.retain(|route| !of(route));
        self.canonical.forget(of);
        for Waiting { queued, job, .. } in self.joining.remove(&workspace_id).unwrap_or_default() {
            self.timed(queued, job);
        }
    }

    /// The member's view of `workspace_id`, joining the workspace first and
    /// holding the keys of what `needs` reads, and every change the mounted
    /// records have not merged yet.
    /// The service's membership of `workspace_id`: the one joined when its
    /// first request arrived, or joined now.
    fn membership(&mut self, workspace_id: WorkspaceId) -> Result<&mut Workspace, ActionCallError> {
        if !self.workspaces.contains_key(&workspace_id) {
            let ahead = self
                .shared
                .joins
                .lock_recover()
                .insert(workspace_id, None)
                .flatten();
            let workspace = match ahead {
                Some(workspace) => workspace,
                None => Workspace::join(&self.host_device, workspace_id, &self.shared)
                    .map_err(|reason| ActionCallError::Unavailable { reason })?,
            };
            self.workspaces.insert(workspace_id, workspace);
        }
        self.workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the workspace was not joined".into(),
            })
    }

    fn refresh(
        &mut self,
        workspace_id: WorkspaceId,
        needs: Needs<'_>,
    ) -> Result<WorkspaceState, ActionCallError> {
        self.membership(workspace_id)?;
        let workspace =
            self.workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ActionCallError::Unavailable {
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
        let holder = if is_part(&route.document) {
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
        let (bytes, heads) = state.document(route)?;
        let record = reads::document_id(&route.document)?;
        let holders: Vec<DocumentRoute> = state
            .records
            .iter()
            .filter(|held| {
                held.app_id == route.app_id
                    && held.cluster_id == route.cluster_id
                    && !is_part(&held.document)
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
                .apply_document(&record, &DocumentMutation::Snapshot(bytes.clone()))
            {
                Ok(()) => {
                    mounted.parts.insert(route.clone(), Some(heads));
                    self.beside.insert(route.clone(), holder.clone());
                    return Ok(holder);
                }
                Err(error) => refusals.push(error.to_string()),
            }
        }
        Err(format!(
            "no record of {} keeps {} beside it: {}",
            route.app_id,
            route.document,
            refusals.join("; ")
        ))
    }

    /// The record at `route` itself, mounted from what the workspace retains.
    fn mount_own(&mut self, route: &DocumentRoute, state: &WorkspaceState) -> Result<(), String> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if !self.mounted.contains_key(route) {
            let (bytes, heads) = state.document(route)?;
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
                &MemberId::Person(self.member.clone()),
                &crate::ReplicaActor::Owner,
                Seed::Existing {
                    record: reads::document_id(&route.document)?,
                    changes: vec![bytes],
                },
            )
            .map_err(|error| error.to_string())?;
            let app = self.lend(app, &route.cluster_id);
            self.mounted.insert(
                route.clone(),
                Mounted {
                    app,
                    merged: Some(heads),
                    parts: HashMap::new(),
                    foreign: std::collections::HashSet::new(),
                    wanted_at: None,
                    used: clock,
                },
            );
        }
        let kept = self.kept_held(kept_untold(route, self.mounted.get(route)));
        let mounted = self
            .mounted
            .get_mut(route)
            .ok_or_else(|| "the record did not mount".to_string())?;
        mounted.used = clock;
        hold_parts(route, mounted, state);
        tell_kept(mounted, state, kept);
        Ok(())
    }

    /// Records of the workspace an app of the catalogue opens: those on the
    /// shared desktop first, then the most recently changed.
    /// The actions `route` lists: someone served within bounds is listed
    /// what their own replica offers them, what the host runs for them and
    /// nothing else; the owner, what the mounted record offers.
    fn listed(
        &mut self,
        workspace_id: WorkspaceId,
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
    fn documents(&self, state: &WorkspaceState, cluster_id: &str) -> Vec<DocumentRoute> {
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
                    document: record,
                };
                if !routes.contains(&route) {
                    routes.push(route);
                }
            }
        }
        routes
            .into_iter()
            .filter(|route| {
                self.catalogue
                    .get(&route.app_id)
                    .is_some_and(|app| opens_as_its_own(app, &route.document))
            })
            .collect()
    }

    fn view_catalogue_page(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        let refuse = || ActionCallError::UnknownAction;
        let (wanted, provider) = exact_view_query(query)?;
        let route_hint = resource_of(provider, "").ok_or_else(refuse)?;
        let link = self.shared.link.get().cloned().ok_or_else(refuse)?;
        let first = link
            .view_read_policy(workspace, caller, &route_hint.app_id, &route_hint.document)
            .ok_or_else(refuse)?;
        let route = resource_of(provider, &first.workspace).ok_or_else(refuse)?;
        let source = link
            .view_document(workspace, caller, &route)
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        let app_id = self
            .catalogue
            .get(&route.app_id)
            .ok_or_else(refuse)?
            .app_id();
        // The document is decoded and the app's resource published in a
        // confined worker, never here.
        let mut views = ConfinedViews::load(app_id, source.bytes)
            .map_err(|reason| ActionCallError::Unavailable { reason })?;
        let descriptors = views.views(wanted, &first).map_err(|_| refuse())?;
        let fresh = link
            .view_read_policy(workspace, caller, &route.app_id, &route.document)
            .ok_or_else(refuse)?;
        if fresh.workspace != first.workspace
            || fresh.access != first.access
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
    fn admits_member(&self, workspace: WorkspaceId, caller: &Caller) -> bool {
        if self.serves_as_owner(caller) {
            return true;
        }
        self.bounded(caller)
            .is_some_and(|who| self.admits_bounded(workspace, &who))
    }

    fn registered_read_catalogue(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        provider: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage> {
        // Only an exact registered-read target is answered here; a listing
        // lists registered reads beside the record's own rows.
        let target = query.target.as_ref()?;
        let mut route = resource_of(provider, "")?;
        let link = self.shared.link.get()?.clone();
        if self.canonical.registers_none(&route.app_id) {
            return None;
        }
        route.cluster_id = link
            .view_read_policy(workspace, caller, &route.app_id, &route.document)?
            .workspace;
        // Document rows retain their existing workspace revision and invocation
        // path. Only an exact registered-read target uses this source revision.
        let mut read = reads::CanonicalRead::prepare(
            &self.catalogue,
            link.as_ref(),
            workspace,
            caller,
            &route,
        )
        .ok()?;
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

    fn invoke(&mut self, workspace_id: WorkspaceId, invocation: InvocationId) -> InvocationReceipt {
        let (caller, request) = match self.claim(invocation) {
            Ok(claimed) => claimed,
            Err(receipt) => return receipt,
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
        let (outcome, unsaved) = match self.invoke_or_ask(workspace_id, &caller, &request) {
            Ok(ran) => ran,
            Err(receipt) => {
                self.log().running.remove(&invocation);
                return receipt;
            }
        };
        log_outcome(&request.target, &outcome);
        let left = outcome
            .as_ref()
            .ok()
            .and_then(|(_, left_at)| left_at.as_ref());
        self.voice(
            workspace_id,
            &caller,
            &request,
            (&session_voice::ended_phase(&outcome), left),
        );
        self.settle(invocation, outcome, unsaved)
    }

    /// Mark `invocation` running, naming its caller and what it asked: a
    /// receipt instead when it is unknown or already finished.
    #[allow(
        clippy::result_large_err,
        reason = "the receipt is the answer the caller records"
    )]
    fn claim(
        &self,
        invocation: InvocationId,
    ) -> Result<(Caller, InvokeRequest), InvocationReceipt> {
        let mut log = self.log();
        let claimed = match log.entries.get(&invocation) {
            None => Err(rejected(invocation, ActionCallError::UnknownInvocation)),
            Some(entry) if entry.update.is_terminal() => {
                // Cancelled before it started.
                Err(InvocationState::new(invocation, entry.update.clone()).receipt())
            }
            Some(entry) => Ok((entry.caller.clone(), entry.request.clone())),
        };
        if claimed.is_ok() {
            log.running.insert(invocation);
        }
        claimed
    }

    /// Record what `outcome` answered for `invocation` and answer it: done
    /// once what it wrote is saved, or the failure or refusal it ended with.
    fn settle(
        &self,
        invocation: InvocationId,
        outcome: Performed,
        unsaved: Option<Unsaved>,
    ) -> InvocationReceipt {
        let outcome = match (outcome, unsaved) {
            (Ok(done), Some(unsaved)) => return self.answer_pending(invocation, done, unsaved),
            (outcome, _) => outcome,
        };
        let mut log = self.log();
        log.running.remove(&invocation);
        let (update, left_at) = match outcome {
            Ok((output, left_at)) => (InvocationUpdate::Succeeded { invocation, output }, left_at),
            Err(Outcome::Failed(error)) if !settles(&error) => {
                // A failure the caller may ask again is not this
                // invocation's answer: forgotten, the same request under the
                // same id runs it again rather than being handed the failure
                // it already has.
                log.forget(&invocation);
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Failed { error },
                };
            }
            Err(Outcome::Failed(error)) => (InvocationUpdate::Failed { invocation, error }, None),
            // Answered accepted while the person was asked: it ends refused,
            // and stays to say so.
            Err(Outcome::Rejected(error))
                if log
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
                log.forget(&invocation);
                return rejected(invocation, error);
            }
        };
        log.answered(invocation, &update, left_at);
        drop(log);
        InvocationState::new(invocation, update).receipt()
    }

    /// The appearance `request` may show: only a bot whose installation
    /// allows it, and only on a record's own actions.
    fn admit_appearance(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<tilo_actions::Appearance>, ActionCallError> {
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
            return Err(ActionCallError::Refused {
                reason: "an appearance is shown only on a record's own actions".into(),
            });
        }
        Ok(Some(admitted))
    }

    /// `target`, its empty resource naming the record's own: a caller that
    /// knows the app but not its code (the desktop asking a connector) names
    /// its actions so.
    fn with_document_resource(
        &self,
        target: &tilo_actions::remote::RemoteActionTarget,
        app_id: &str,
    ) -> tilo_actions::remote::RemoteActionTarget {
        let mut target = target.clone();
        if target.address.resource.is_empty()
            && let Some(definition) = self.catalogue.get(app_id)
        {
            definition
                .resource_type
                .as_str()
                .clone_into(&mut target.address.resource);
        }
        target
    }

    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    /// Invocation `request`: what it answered, and where it left its actor in the
    /// record it changed.
    fn perform(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let appearance = self
            .admit_appearance(workspace_id, caller, request)
            .map_err(Outcome::Rejected)?;
        if let Some((named, app)) = application_provider(&request.target.provider) {
            return self
                .show_on_layout(workspace_id, caller, (named, app), request)
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
                return Err(Outcome::Rejected(ActionCallError::StaleRevision));
            }
            return self.open_in_workspace(workspace_id, caller, request, (named, &state));
        }
        let workspace = workspace.ok_or_else(unknown_action)?;
        let route = resource_of(&target.provider, &workspace)
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .ok_or_else(unknown_action)?;
        let target = &self.with_document_resource(target, &route.app_id);
        let arguments = invoke_arguments(&request.argument);
        // What the record publishes, its own API runs. What only its
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
            return Err(Outcome::Rejected(ActionCallError::StaleRevision));
        }
        let mounted = self.mount(&route, &state).map_err(|_| unknown_action())?;
        if target.address.action == ui::tilo_resource::share::SHARE_LINK {
            // Sharing asks the record for everything it lists, which is what
            // the link is chosen from.
            // The kind of the resource the link names, from the action
            // asked for, resolved by its address: never a survey of all the
            // record publishes, which grows with everything it holds.
            let kind = mounted
                .app
                .descriptor(&target.address)
                .map(|descriptor| descriptor.resource_kind)
                .ok_or_else(unknown_action)?;
            let at = (&route, &state, kind);
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
        let binding = bounded::ApprovalBinding::of(workspace_id, caller, request);
        let confirmed = confirms(
            &descriptor.contract.confirmation,
            request.confirmation.as_ref(),
            |token| self.shared.approvals.take(token, &binding),
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
            .map_err(|error| api_outcome(&error))?;
        // Where the action left its actor, for the API that shows it
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
        workspace_id: WorkspaceId,
        caller: &Caller,
        named: &str,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<ActionOutput, Outcome> {
        if WorkspaceId::parse(named) != Some(workspace_id) {
            return Err(unknown_action());
        }
        let who = if self.serves_as_owner(caller) {
            None
        } else {
            Some(self.bounded(caller).ok_or_else(unknown_action)?)
        };
        let app = self.asked_app(target).ok_or_else(unknown_action)?;
        self.open_new(workspace_id, named, &app, who.as_ref())
    }

    /// The app a workspace's `app.<app>#open` (or `#start`) names, when this
    /// host runs it.
    fn asked_app(&self, target: &tilo_actions::remote::RemoteActionTarget) -> Option<String> {
        target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            // Its records have no window to show: starting the app is opening it.
            .filter(|_| {
                target.address.action == APPLICATION_OPEN_ACTION
                    || target.address.action == APPLICATION_START_ACTION
            })
            // Files is the workspace's own folder, not an app this host runs.
            .filter(|app| self.catalogue.has(app) || *app == tilo_fs::tree_document::FILES_APP)
            .map(str::to_owned)
    }

    /// Commit what the owner's action changed, each record on its own
    /// route: a part it started is held beside the record it acted on. The
    /// action is done once the host holds them ([`Self::saving`]). The error
    /// is why a commit failed.
    fn commit_taken(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        taken: Vec<(DocumentRoute, DocumentMutation)>,
    ) -> Result<(), String> {
        let holder = self.holder_of(route);
        for (changed, mutation) in taken {
            if changed != holder {
                self.beside.insert(changed.clone(), holder.clone());
            }
            self.commit(workspace_id, &changed, &mutation)?;
        }
        Ok(())
    }

    /// Whether `caller` is the host's own person, whom the service acts for.
    fn serves_as_owner(&self, caller: &Caller) -> bool {
        caller.acting().person_id() == Some(self.member.as_str())
    }

    /// Whether `caller` is the host's own person or an agent one of their
    /// devices handed its session to: who may open records, show them on the
    /// shared layout and run commands in the workspace's shell.
    fn acts_for_owner(&self, caller: &Caller) -> bool {
        acts_for(caller, &self.member)
    }

    fn try_registered_read(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<ActionOutput>, Box<Outcome>> {
        let Some(hint) = resource_of(&request.target.provider, "") else {
            return Ok(None);
        };
        let Some(link) = self.shared.link.get() else {
            return Ok(None);
        };
        let Some(policy) = link.view_read_policy(workspace, caller, &hint.app_id, &hint.document)
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
            // ordinary document API. Unregistered effects stay refused there.
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
            return Err(Box::new(Outcome::Rejected(ActionCallError::Refused {
                reason: "the committed source changed".into(),
            })));
        }
        let arguments = match &request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::Text(_) => {
                return Err(Box::new(Outcome::Rejected(
                    ActionCallError::InvalidRequest {
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
        .map_err(|error| Box::new(api_outcome(&error)))
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
        workspace_id: WorkspaceId,
        workspace: &str,
        app: &str,
        who: Option<&bounded::Bounded>,
    ) -> Result<ActionOutput, Outcome> {
        let refused = |reason: String| Outcome::Rejected(ActionCallError::Refused { reason });
        let (creator, actor, permissions) = match who {
            None => (self.member.clone(), crate::ReplicaActor::Owner, None),
            Some(who) => {
                let policy = self
                    .policy_for(workspace_id, who, workspace, None)
                    .map_err(Outcome::Rejected)?;
                let permissions =
                    bounded::bounded_permissions(&policy, who).map_err(Outcome::Rejected)?;
                if permissions.answer(&TreePath::root().app(app), ActionKind::Undoable)
                    != Answer::Allow
                {
                    return Err(Outcome::Rejected(ActionCallError::NotPermitted));
                }
                let actor = bounded::member_actor(&policy, who).map_err(Outcome::Rejected)?;
                (who.person.clone(), actor, Some(permissions))
            }
        };
        let (route, genesis) = if self.catalogue.placed(app).is_some() {
            self.placed_route(workspace_id, workspace, app)?
        } else {
            // An app that keeps one record per workspace opens that record: the
            // one the workspace holds when it has it, founded only when its host
            // answered it holds none.
            if self
                .catalogue
                .get(app)
                .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_resource)
            {
                let route = DocumentRoute {
                    app_id: app.to_owned(),
                    cluster_id: workspace.to_owned(),
                    document: String::new(),
                };
                if self.held_or_owed(workspace_id, &route)? {
                    return Ok(ActionOutput {
                        value: Some(serde_json::json!({
                            "provider": record_provider(&route),
                            "record": route.document,
                        })),
                        resources: Vec::new(),
                    });
                }
            }
            let hosted = ConfinedReplica::open(
                &self.catalogue,
                app,
                workspace,
                &MemberId::Person(creator),
                &actor,
                Seed::New,
            )
            .map_err(|error| refused(error.to_string()))?;
            let hosted = self.lend(hosted, workspace);
            let route = DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                document: hosted.wire_document().to_owned(),
            };
            let genesis = hosted
                .take_mutation()
                .map_err(|error| refused(error.to_string()))?;
            (route, genesis)
        };
        if let (Some(permissions), Some(mutation)) = (&permissions, &genesis) {
            let unwritable =
                bounded::refused_writes(permissions, (app, None), None, mutation, (None, false))
                    .map_err(|reason| commit_failed(reason, false))?;
            if !unwritable.is_empty() {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        }
        // When the host device takes part in the workspace's documents, it opens a
        // new record's document for the member and hands it the keys, so the
        // first change goes out sealed instead of waiting for keys.
        if genesis.is_some()
            && let Some(keys) = self
                .shared
                .link
                .get()
                .and_then(|link| link.host_keys(&route, true))
        {
            let keys = keys.map_err(|reason| {
                Outcome::Rejected(ActionCallError::Unavailable {
                    reason: format!("the host holds no keys to a new {app} record: {reason}"),
                })
            })?;
            self.workspaces
                .get(&workspace_id)
                .ok_or_else(|| refused("the workspace was not joined".into()))?
                .peer
                .send(Cmd::InstallGroupKeys(route.clone(), keys))
                .map_err(|closed| refused(closed.to_string()))?;
        }
        if let Some(mutation) = genesis {
            self.commit(workspace_id, &route, &mutation)
                .map_err(refused)?;
        }
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "provider": record_provider(&route),
                "record": route.document,
            })),
            resources: Vec::new(),
        })
    }
}

/// Whether `given`, from the host's own person on their own device, confirms
/// an action asking `policy`: their own word, the phrase it names, or a
/// token naming their answer that the host issued for this very invocation
/// (`approved` spends it).
fn confirms(
    policy: &ConfirmationPolicy,
    given: Option<&Confirmation>,
    approved: impl FnOnce(&str) -> bool,
) -> bool {
    match (policy, given) {
        (_, Some(Confirmation::Approved(token))) => approved(token),
        (ConfirmationPolicy::Phrase { phrase }, Some(Confirmation::Phrase(given))) => {
            phrase == given
        }
        (ConfirmationPolicy::Phrase { .. }, _) | (_, None) => false,
        (_, Some(Confirmation::Confirmed | Confirmation::Phrase(_))) => true,
    }
}

fn exact_view_query(
    query: &CatalogueQuery,
) -> Result<(&tilo_actions::remote::ViewResourceQuery, &str), ActionCallError> {
    let wanted = query
        .view_resource
        .as_ref()
        .ok_or(ActionCallError::UnknownAction)?;
    let provider = query
        .provider
        .as_deref()
        .ok_or(ActionCallError::UnknownAction)?;
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
        return Err(ActionCallError::UnknownAction);
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
    if caller.acting().is_person() {
        return true;
    }
    if !caller.acting().is_agent() {
        return false;
    }
    if caller.is_session_delegated() {
        return caller.acting_for_person().is_some();
    }
    caller.user_public_key().is_some() && caller.device_public_key().is_some()
}

/// The receipt of an invocation the host turns away outright, with why.
fn rejected(invocation: InvocationId, error: ActionCallError) -> InvocationReceipt {
    InvocationReceipt {
        invocation,
        status: ReceiptStatus::Rejected { error },
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

/// Whether `record` is one of `app`'s own records, which its replica opens. An app that keeps one
/// record per workspace opens that one, and what it keeps as records of its own beside it (a
/// chain's parts); a person's record of some kind, kept beside the app's for that person (Files'
/// tree of the workspace's folder, and the chunks that tree is kept in), is read where it is kept,
/// never mounted as the app's, which refuses it as not the app's workspace record.
fn opens_as_its_own(app: &ui::application::ApplicationDefinition, record: &str) -> bool {
    // The empty route is the app's own record where it keeps one per workspace, and its index of
    // the workspace's named records where it keeps none: the index is read where it is kept,
    // never mounted as the app's record.
    if record.is_empty() {
        return app.keeps_workspace_resource();
    }
    // A tree's own record and its chunks are one folder, read record by record where it is kept.
    if fs_protocol::tree::chunk_of_record(record).is_some() {
        return false;
    }
    !app.keeps_workspace_resource() || collab_types::person_resource_type(record).is_none()
}

#[cfg(test)]
mod tests;
