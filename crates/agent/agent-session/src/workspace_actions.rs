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
//! ([`bounded`]): in a replica of their own on their current access, a
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
//! ([`WorkspaceDocumentsLink::serving_keys`]), for each document a request reads or
//! acts on, fresh for every act.

mod acting;
mod arguments;
mod asked_in_turn;
mod bounded;
mod call_wait;
mod canonical;
mod carried;
mod catalogue;
mod catch_up;
mod change;
mod content;
mod direct;
mod feedback;
mod files_index;
mod files_listing;
mod follow;
mod found;
mod found_files;
#[cfg(feature = "test-support")]
pub mod held_back;
mod index;
mod invocation_log;
mod invocations;
mod job;
pub mod jobs;
mod joining;
mod keys;
mod layout_window;
mod link;
mod listing;
mod member;
mod naming;
mod opened;
mod opened_file;
mod outcome;
mod packaged;
mod pages;
mod part_load;
mod part_wait;
mod person_ask;
mod placed;
mod publish;
mod queue;
mod reads;
mod run_context;
mod search_access;
mod search_activity;
#[cfg(feature = "test-support")]
pub mod slowed;
use search_activity::SearchActivity;
mod sealed;
use sealed::{GaveUp, sealed_for};
mod session_voice;
mod share;
mod terminal;
mod timing;
mod tree;
pub use crate::worker::PersonRegion;
pub(super) use arguments::invoke_arguments;
pub use content::MAX_PUT_BYTES;
pub use link::{
    AskedInTurn, AsksInConversation, FeedbackReviewer, HostDevice, PackagedCall, ViewDocument,
    ViewReadPolicy, WorkspaceDocumentsLink,
};
pub use terminal::{
    Opened, Opening, Recorder, ShellRan, TERMINAL_APP, Watching, WorkspaceShell, person_of,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use tilo_peer_state::workspace_documents::{DeviceDocuments, DeviceWrites, DocumentRead};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Event, Link, Outbox};

use crate::apps::{AppCatalogue, Seed};
use crate::record_actions::{Outcome, api_outcome, resource_of, resource_provider};
use crate::service::CatalogueFactory;
use crate::worker::{ConfinedReplica, ConfinedViews};
use crate::workspace_documents::{Reads, WorkspaceDocuments};
use catch_up::caught_up_with_own;
use collab_types::DocumentMutation;
use invocation_log::InvocationLog;
use jobs::Jobs;
use joining::Waiting;
use keys::{Keys, catalogue_keys};
use naming::{is_part, named, named_document};
use outcome::{
    commit_failed, confirms, exact_view_query, opens_as_its_own, rejected, speaks_for_a_member,
};
use part_wait::Invoked;
use run_context::{caller_context, said_invocation_title};
use tilo_actions::InteractionPhase;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION,
    ActionCallError, ActorPlace, Caller, CataloguePage, CatalogueQuery, CatalogueRow, Completer,
    Confirmation, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionMessage, application_provider,
    application_provider_resource, pending, provider_workspace,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate, MemberId};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use timing::{timing_done, timing_start};
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
/// A request that waited for the worker, or ran, at least this long is
/// logged: every request of a workspace waits behind the one before it, and
/// behind the requests of the other workspaces its thread serves.
const SLOW_JOB: Duration = Duration::from_secs(2);

struct Shared {
    log: Mutex<InvocationLog>,
    link: OnceLock<Arc<dyn WorkspaceDocumentsLink>>,
    jobs: Mutex<Option<Jobs>>,
    /// How the service joins workspaces, once its thread runs.
    own_device: OnceLock<HostDevice>,
    /// This device's own documents, as its service holds them: what the
    /// workspaces' records are read from and written through.
    reads: OnceLock<Arc<dyn DeviceDocuments>>,
    writes: OnceLock<Arc<dyn DeviceWrites>>,
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
    /// The content steps served beside the service's own ([`content::ContentSteps`]).
    steps: Arc<content::ContentSteps>,
    /// The uploads callers have open.
    puts: tilo_actions::remote::HostedContentPuts,
    /// The answers people gave to invocations run for them.
    approvals: bounded::Approvals,
    /// What asks an agent's person in the conversation its turn is in.
    in_conversation: std::sync::RwLock<Option<Arc<dyn AsksInConversation>>>,
    /// Records whose name in their app's index waits to be read again.
    renaming: Mutex<HashSet<DocumentRoute>>,
    /// What search read of the documents, counted for the timing lines and for the tests that
    /// hold a query to the index instead of to what it is built from.
    search: SearchActivity,
    /// The agents' sessions here, which sign a change an action made for each.
    agent_sessions: OnceLock<crate::grants::AgentSessions>,
}

impl Shared {
    /// This device's documents, as its service holds them: `None` before
    /// [`WorkspaceActions::attach`] or on a device that serves no workspaces.
    fn reads(&self) -> Option<Reads> {
        self.reads.get().map(|reads| Reads::new(Arc::clone(reads)))
    }

    /// This device's documents, written as its service writes them.
    fn writes(&self) -> Option<Arc<dyn DeviceWrites>> {
        self.writes.get().map(Arc::clone)
    }
}

/// The host's page actions over every record of its workspaces.
#[derive(Clone)]
pub struct WorkspaceActions {
    shared: Arc<Shared>,
    catalogue: CatalogueFactory,
}

impl WorkspaceActions {
    /// A service mounting apps from `catalogue`; it answers once
    /// [`Self::attach`] gives it the host's documents.
    #[must_use]
    pub fn new(catalogue: CatalogueFactory) -> Self {
        Self {
            shared: Arc::new(Shared {
                log: Mutex::new(InvocationLog::default()),
                link: OnceLock::new(),
                jobs: Mutex::new(None),
                own_device: OnceLock::new(),
                reads: OnceLock::new(),
                writes: OnceLock::new(),
                member: OnceLock::new(),
                joins: Mutex::default(),
                stores: Mutex::default(),
                steps: Arc::new(content::ContentSteps::default()),
                puts: tilo_actions::remote::HostedContentPuts::default(),
                approvals: bounded::Approvals::default(),
                in_conversation: std::sync::RwLock::default(),
                renaming: Mutex::default(),
                agent_sessions: OnceLock::new(),
                search: SearchActivity::default(),
            }),
            catalogue,
        }
    }

    /// Start serving `link`'s workspaces on the service's threads, one for
    /// each worker of the app worker pool ([`crate::worker::pool_size`]);
    /// each worker starts when a document is first held in it. A second call is
    /// ignored.
    pub fn attach(&self, link: Arc<dyn WorkspaceDocumentsLink>) -> Result<(), String> {
        if self.shared.link.set(Arc::clone(&link)).is_err() {
            return Ok(());
        }
        // The device's documents, as its service holds them: the workspaces'
        // records are read and written through them, never through a copy in
        // the session's replica.
        if let Some(reads) = link.documents() {
            let _ = self.shared.reads.set(reads);
        }
        if let Some(writes) = link.writes() {
            let _ = self.shared.writes.set(writes);
        }
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

    /// How many documents search opened to read what their apps publish, since the service
    /// started: a query answered from the index opens none, however much the workspace holds.
    #[must_use]
    pub fn search_documents_opened(&self) -> u64 {
        self.shared.search.opened.load(Ordering::Relaxed)
    }

    /// How many documents were read for the search index between requests, as they landed:
    /// what keeps a find from opening them while the person waits.
    #[must_use]
    pub fn search_documents_warmed(&self) -> u64 {
        self.shared.search.warmed.load(Ordering::Relaxed)
    }

    /// How many requests were served while the index was reading a document: a document read
    /// for search never holds the thread a request waits on.
    #[must_use]
    pub fn search_served_beside_reading(&self) -> u64 {
        self.shared.search.beside_reading.load(Ordering::Relaxed)
    }

    /// How many documents the index is reading now: none once the index holds every document
    /// the workspace's writes have landed.
    #[must_use]
    pub fn search_documents_reading(&self) -> u64 {
        self.shared.search.reading.load(Ordering::Relaxed)
    }

    /// How many of the workspace's folder documents a find read while it waited: none once the
    /// folder's index is kept as its chunks land.
    #[must_use]
    pub fn search_files_chunks_read(&self) -> u64 {
        self.shared.search.files_chunks.load(Ordering::Relaxed)
    }

    /// How many files a find read the words of while it waited, from a chunk or the content
    /// store: what a folder kept without an index costs on every query.
    #[must_use]
    pub fn search_files_read(&self) -> u64 {
        self.shared.search.files_read.load(Ordering::Relaxed)
    }

    /// How many files were read for the folder's index between requests, as they landed.
    #[must_use]
    pub fn search_files_warmed(&self) -> u64 {
        self.shared.search.files_warmed.load(Ordering::Relaxed)
    }

    /// How many permission reads a find made for its caller: one for each answer it returned
    /// that was not kept yet, and none once the caller's access is.
    #[must_use]
    pub fn search_permissions_read(&self) -> u64 {
        self.shared.search.permissions.load(Ordering::Relaxed)
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

    /// Another's write landed in `route` of the store this service reads, as its host just read
    /// it there: the replica holding the record merges it before the next request of the
    /// workspace, which is then made on what the host read. Its own session tells the service of
    /// the landing too, a moment later, and a request taken before that acted on what the
    /// record held before it: a person's answer the host had read was not in the conversation the
    /// host's next write went to.
    pub fn landed(&self, workspace_id: WorkspaceId, route: DocumentRoute) {
        let _ = self.submit(Job::Landed(workspace_id, route));
    }

    /// Stop the service thread; later requests are refused as unavailable.
    pub fn shutdown(&self) {
        self.shared.jobs.lock_recover().take();
    }

    /// Whether `caller` is the host's own person, or an agent they
    /// certified.
    fn authorized(&self, caller: &Caller) -> bool {
        let Some(link) = self.shared.link.get() else {
            return false;
        };
        acts_for(caller, &hex::encode(link.user_public_key()))
    }

    /// That an agent acting with its own key asks for `request` under that key, checked as a
    /// host checks one that comes over the network
    /// ([`tilo_server_protocol::invocation::verify_agent_invocation`]): the signature, that it
    /// covers this very request, its freshness, the certificate's chain to its person's root
    /// and its revocation, that the key is an agent's grant reaching this workspace and that it
    /// is the key the caller was admitted as. A device's own key never signs for an agent, and
    /// everyone else is admitted as they are.
    fn signed_by_agent(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(), String> {
        let (Some(person), Some(key)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return Ok(());
        };
        // A sub-agent works under the key of the agent that heads its line.
        let Some(agent) = caller
            .approved_agent()
            .or_else(|| caller.acting().agent_id())
        else {
            return Ok(());
        };
        let revocations = self
            .shared
            .link
            .get()
            .map(|link| link.revocations())
            .ok_or("the host's workspace actions are not running")?;
        tilo_server_protocol::invocation::verify_agent_invocation(
            &workspace_id.to_string(),
            request,
            (person, agent, key),
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map(drop)
        .map_err(|error| format!("{}: {error}", error.code()))
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
        // An agent asking the person it works for something in its own words.
        let asks = tilo_actions::remote::is_ask(&request.target);
        let member_request =
            (on_resource || on_workspace || shows || asks) && speaks_for_a_member(caller);
        link_request || member_request || self.authorized(caller)
    }

    /// Start joining `workspace_id` unless the service has: the thread
    /// waits only for what is still missing when it reaches the request.
    /// Joining sends the subscription and returns; the session runs on the
    /// member's runtime.
    fn join_ahead(&self, workspace_id: WorkspaceId) {
        let Some(own_device) = self.shared.own_device.get() else {
            return;
        };
        let mut joins = self.shared.joins.lock_recover();
        if joins.contains_key(&workspace_id) {
            return;
        }
        match Workspace::join(own_device, workspace_id, &self.shared) {
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
        self.log()
            .entries
            .get(id)
            .filter(|entry| caller.continues(&entry.caller))
            .map(|entry| {
                InvocationState::new(*id, entry.update.clone())
                    .linked(entry.link.clone())
                    .with_untrusted(tree::answers_from_outside(&entry.request.target))
            })
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
            RemoteActionMessage::Observe {
                correlation,
                invocation,
                wait_ms,
                seen,
            } => self.follow(caller, correlation, invocation, seen, wait_ms),
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
            // One document, read as every reader of it is answered: the same answer a person's
            // device gives, from what the service itself holds.
            RemoteActionMessage::ResourceRead {
                correlation, read, ..
            } => {
                let reads: Arc<dyn tilo_actions::remote::ResourceReads> = Arc::new(self.clone());
                reads.answer_read(caller, correlation, read)
            }
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::ResourceReading { .. }
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::Confirm { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::ContentLinkResponse { .. }
            | RemoteActionMessage::ChangesResponse { .. }
            | RemoteActionMessage::ChangedResponse { .. }
            | RemoteActionMessage::SurfaceResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return None,
        })
    }

    /// What the workspace's search finds for `request` ([`tilo_actions::remote::search::FIND`]),
    /// as `caller` may read it, answered as the invocation's receipt.
    fn search_request(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        use tilo_actions::remote::search;
        let invocation = request.invocation;
        let query = match search::Query::of(request) {
            Ok(query) => query,
            Err(error) => {
                return Pending::ready(RemoteActionMessage::Receipt(search::receipt(
                    invocation,
                    Err(error),
                )));
            }
        };
        let (done, answer) = pending();
        let job = Job::Search {
            workspace_id,
            caller: caller.clone(),
            needle: query.needle,
            within: query.within,
            done,
        };
        if let Err(Job::Search { done, .. }) = self.submit(job) {
            done.complete(Err(ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(move |found| RemoteActionMessage::Receipt(search::receipt(invocation, found)))
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
        // So is the workspace's search, run on the service's thread as every reader is.
        if tilo_actions::remote::search::is_find(&request.target) {
            return self.search_request(workspace_id, caller, &request);
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
        if let Err(reason) = self.signed_by_agent(workspace_id, caller, &request) {
            return receipt(ReceiptStatus::Rejected {
                error: ActionCallError::Refused { reason },
            });
        }
        {
            let mut log = self.log();
            let held = log.entries.get(&invocation);
            if let Some(entry) = held
                && (!caller.continues(&entry.caller) || !entry.request.same_invocation(&request))
            {
                return receipt(ReceiptStatus::Rejected {
                    error: ActionCallError::InvocationConflict,
                });
            }
            // A failure the caller was told to ask again about is made once the
            // same request comes back under its id: the same call makes it,
            // never a second copy of it.
            if held.is_some_and(|entry| entry.update.asks_again()) {
                log.forget(&invocation);
            } else if let Some(entry) = log.entries.get_mut(&invocation) {
                // Asked again on the connection that replaced its own: it is this one's now.
                entry.caller.clone_from(caller);
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
            .filter(|entry| caller.continues(&entry.caller))
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
    /// Shared with the thread an operation on it goes on in, when it leaves this one
    /// ([`call_wait`]).
    app: Arc<ConfinedReplica>,
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
    /// What was written in the workspace since it last caught up, when
    /// anything was: it takes that up when it is next used
    /// ([`Worker::brought_up_to_date`]), never for a write it plays no part in.
    behind: Option<HashSet<DocumentRoute>>,
    used: u64,
}

/// The replica a write of this service was made in: the document mounted for its own person,
/// or a caller's own replica within bounds.
#[derive(PartialEq, Eq)]
enum Writer {
    Mounted(DocumentRoute),
    Bounded(bounded::BoundedKey),
}

/// What running an action came to: its answer and where it left its actor.
pub(super) type Performed = Result<(ActionOutput, Option<ActorPlace>), Outcome>;

/// A record saved, and the heads it was saved at, sorted.
pub(super) type Saved = (Vec<u8>, Vec<[u8; 32]>);

/// What a replica merges of a record, and the heads the record stands at
/// once it has.
type Update = (DocumentMutation, Vec<[u8; 32]>);

/// A workspace as the session's replica holds it when a request is served.
#[derive(Clone)]
struct WorkspaceState {
    workspace: Option<String>,
    /// Its documents not held open, as the replica last listed them: read
    /// from whenever a request needs them, never listed again per request.
    documents: Arc<[DocumentRoute]>,
    reads: Reads,
    /// How many writes landed in it.
    revision: u64,
    /// How many writes landed in each app's documents.
    apps: Arc<HashMap<String, u64>>,
}

impl WorkspaceState {
    /// The revision a page of `provider`, or an action on it, is read at: how many writes
    /// landed in what it reads. A resource's page moves with its app's writes alone and the
    /// workspace's own rows with its folder's, so a busy app (a mail import) never makes another
    /// app's page or action stale; a listing of every resource moves with any write.
    fn revision_of(&self, provider: Option<&str>) -> String {
        match provider.and_then(revision_scope) {
            Some(app) => format!("{app}@{}", self.app_revision(&app)),
            None => self.revision.to_string(),
        }
    }

    fn app_revision(&self, app: &str) -> u64 {
        self.apps.get(app).copied().unwrap_or(0)
    }

    /// Whether `named`, the revision a page was read at ([`Self::revision_of`]), has moved on.
    fn stale(&self, named: Option<&str>) -> bool {
        let Some(named) = named else {
            return false;
        };
        match named.rsplit_once('@') {
            Some((app, count)) => count.parse::<u64>().ok() != Some(self.app_revision(app)),
            None => named != self.revision.to_string(),
        }
    }
}

/// The app whose writes move a page of `provider`: a resource's own app, or the workspace's
/// folder for the workspace's own rows. `None` for a provider that names neither.
fn revision_scope(provider: &str) -> Option<String> {
    resource_of(provider, "")
        .map(|route| route.app_id)
        .or_else(|| provider_workspace(provider).map(|_| fs_protocol::tree::FILES_APP.to_owned()))
}

/// How many reads of a document out of the replica the worker made since the
/// process started: what a refresh merges into what it holds mounted
/// ([`WorkspaceState::update`]), a document mounted or held whole
/// ([`WorkspaceState::document`]), and a landing decided ([`Workspace::pump`]).
/// A test reads it to prove that work follows what a write changed and what
/// a request names, not what the workspace holds.
static REPLICA_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Count one read of a document out of the replica ([`REPLICA_READS`]).
fn count_read() {
    REPLICA_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// [`REPLICA_READS`]: how many reads of a document the worker did so far in this process.
#[must_use]
pub fn replica_reads() -> u64 {
    REPLICA_READS.load(std::sync::atomic::Ordering::Relaxed)
}

impl WorkspaceState {
    fn holds(&self, route: &DocumentRoute) -> bool {
        self.documents.iter().any(|held| held == route)
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
        count_read();
        Ok(match self.reads.read(route, held)? {
            DocumentRead::Whole { heads, bytes, .. } => {
                Some((DocumentMutation::Snapshot(bytes), heads))
            }
            DocumentRead::Changes { heads, bytes, .. } => {
                Some((DocumentMutation::Incremental(bytes), heads))
            }
            DocumentRead::Unchanged(_) | DocumentRead::Absent | DocumentRead::Pending => None,
        })
    }

    /// The record at `route`, saved, with its heads.
    fn document(&self, route: &DocumentRoute) -> Result<Saved, String> {
        count_read();
        match self.reads.read(route, None)? {
            DocumentRead::Whole { heads, bytes, .. } => Ok((bytes, heads)),
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

/// The service's membership of one workspace, on the host device's one
/// session ([`member`]); it leaves the workspace when dropped.
struct Workspace {
    id: WorkspaceId,
    peer: Link,
    member: Arc<member::Member>,
    /// This device's documents, as its service holds them: what this
    /// workspace's records are read and written through.
    reads: Reads,
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
    /// How many writes this service handed the session to each document whose landing it has
    /// not seen yet ([`Worker::commit`]), by the device they land as: its own writes, which the
    /// replicas it holds took up as they were made, and which it read to make, so their landing
    /// is neither merged again nor leaves anything undecided. Those it made for an agent land as
    /// the agent's device, signed by the agent's session, and are as much its own: taken for
    /// another's, each part an import started was read out whole as it landed, most of a second
    /// before the import's next list could start. A document stays until the refresh after its
    /// last write landed, as the segments of a write too large for one frame land one by one.
    unlanded: HashMap<(DocumentRoute, [u8; 32]), usize>,
    /// The documents a write landed in since the last refresh took them that this service did
    /// not make: what a refresh has replicas merge.
    landed_from_others: HashSet<DocumentRoute>,
    /// The workspace's documents in the session's replica, once a write to one
    /// named the workspace.
    documents: Option<WorkspaceDocuments>,
    /// The files of its folder, kept as the tree's chunks land ([`files_index`]).
    files: files_index::FilesIndex,
    /// What each caller of a find here may read, until a write lands in the workspace
    /// ([`search_access`]).
    readers: HashMap<search_access::Reader, search_access::SearchAccess>,
    /// What each document's app lists at its top level, as a find of the workspace's own person
    /// last read it: what names a hit the app anchors on another resource.
    listings: HashMap<DocumentRoute, Vec<CatalogueRow>>,
    /// The resource a hit at a document was named as, by the resource id the hit names.
    named: HashMap<(DocumentRoute, String), Option<tilo_actions::remote::Found>>,
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
        own_device: &HostDevice,
        workspace_id: WorkspaceId,
        shared: &Arc<Shared>,
    ) -> Result<Self, String> {
        let member = Arc::clone(
            shared
                .member
                .get_or_init(|| member::Member::start(own_device, shared)),
        );
        member.join(workspace_id)?;
        let reads = shared
            .reads()
            .ok_or("this device holds no documents to join a workspace with")?;
        Ok(Self {
            sessions: session_voice::Sessions::new(
                workspace_id,
                own_device.endpoint.server_cert.device_public_key,
            ),
            id: workspace_id,
            peer: member.peer().clone(),
            member,
            reads,
            ready: false,
            when_opened: false,
            asked: HashSet::new(),
            answered: HashSet::new(),
            absent: HashSet::new(),
            not_yours: HashSet::new(),
            gave_up: GaveUp::default(),
            undecided: BTreeSet::new(),
            unlanded: HashMap::new(),
            landed_from_others: HashSet::new(),
            documents: None,
            files: files_index::FilesIndex::default(),
            readers: HashMap::new(),
            listings: HashMap::new(),
            named: HashMap::new(),
            changes: ui::history_source::feed::ChangeLog::new(),
            run_index: invocations::Index::default(),
            watchers: change::Watchers::default(),
        })
    }

    /// The workspace as the session's replica holds it now.
    fn state(&mut self) -> WorkspaceState {
        // A workspace this session joined after its documents were written has had no write
        // land here yet, and a find over it would answer with nothing: its documents are in the
        // replica, named where the workspace keeps them, so the index is built over those too.
        if self.documents.is_none()
            && let Some(cluster) = self.cluster_id(self.id)
        {
            self.documents = Some(WorkspaceDocuments::new(&cluster, self.reads.clone()));
        }
        let reads = self.reads.clone();
        match &mut self.documents {
            Some(kept) => WorkspaceState {
                workspace: Some(kept.workspace().to_owned()),
                documents: kept.known(),
                reads,
                revision: kept.landings(),
                apps: kept.app_landings(),
            },
            None => WorkspaceState {
                workspace: None,
                documents: Vec::new().into(),
                reads,
                revision: 0,
                apps: Arc::default(),
            },
        }
    }

    /// Count a write this service handed over to `route`, landing as `author`
    /// ([`Self::unlanded`]).
    fn handed(&mut self, route: &DocumentRoute, author: [u8; 32]) {
        let handed = self.unlanded.entry((route.clone(), author)).or_default();
        *handed = handed.saturating_add(1);
    }

    /// The documents a write landed in since the last refresh took them, by
    /// another device or another service than this one: the only ones a
    /// refresh has to merge into what it holds mounted. This service's own
    /// writes reached every replica it holds as they were made
    /// ([`Worker::behind_all_but`]).
    fn take_changed(&mut self) -> HashSet<DocumentRoute> {
        self.unlanded.retain(|_, handed| *handed > 0);
        std::mem::take(&mut self.landed_from_others)
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
    ///
    /// `pump` is whether the changes that landed are taken up first: a request applied again to
    /// take an answer it already has reads none of them.
    fn wait_for(
        &mut self,
        workspace_id: WorkspaceId,
        needs: Needs<'_>,
        link: Option<&dyn WorkspaceDocumentsLink>,
        pump: bool,
    ) -> Result<(), ActionCallError> {
        // A request applied again to take the answer of the operation it handed off the thread
        // reads no change: the operation ran already, and what landed since is merged by the
        // next request. Reading it here would read a landing in the middle of one request.
        if pump {
            self.pump(workspace_id);
        }
        if !self.ready {
            return Err(ActionCallError::Unavailable {
                reason: "the host has not joined the workspace yet".into(),
            });
        }
        // Keys the host device holds are handed over as soon as a route needs
        // them: the record acted on at once (fresh every time), and every
        // route that arrived sealed while the request waited. A resource the
        // workspace holds none of is not asked for: the change this request
        // makes founds it and opens its document's group with it
        // ([`publish::Worker::hand_over`]). Asking anyway would refuse what
        // the request itself brings, for keys no device can hand yet.
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
            let held = self.holds(&route);
            if held {
                self.take_host_keys(vec![route], needs, link, &mut handed)?;
            }
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
    /// ([`WorkspaceDocumentsLink::serving_keys`]), noting each in `handed`. The
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
            match link.serving_keys(&route, false) {
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
        if let Some(kept) = &self.documents {
            return Some(kept.workspace().to_owned());
        }
        self.reads
            .workspaces()
            .into_iter()
            .find(|workspace| WorkspaceId::parse(workspace) == Some(workspace_id))
    }
}

struct Worker {
    own_device: HostDevice,
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
    /// Writes waiting off the thread for the part they change to arrive
    /// ([`part_wait`]).
    part_waits: part_wait::PartWaits,
    /// Requests whose operation goes on off the thread, and what waits for them ([`call_wait`]).
    call_waits: call_wait::CallWaits,
    /// An agent's read was answered since the queue last heard of one
    /// ([`queue::Queue::agent_read_answered`]).
    agent_read_answered: bool,
    /// Callers' replicas taking in their parts off the thread ([`part_load`]).
    part_loads: part_load::PartLoads,
    /// The caller's device whose request it serves now ([`publish::Worker::writer`](publish)).
    writing_as: Option<Caller>,
    clock: u64,
    /// What the apps call the records the host changes, read in a confined
    /// worker kept for the next ([`publish`]).
    titles: crate::worker::ConfinedTitles,
    /// What each of Chat's documents keeps of direct conversations ([`direct`]).
    said_directly: direct::ReadOnce,
    /// The writes the job being served handed to the session, by workspace:
    /// an invocation is answered done only once the host holds them
    /// ([`publish::Worker::saving`]).
    handed: HashMap<WorkspaceId, Vec<publish::Written>>,
    /// The registered reads listings offer, kept between them
    /// ([`canonical`]).
    canonical: canonical::CanonicalReads,
    /// When each record's name was last read for its app's index ([`publish`]).
    named: HashMap<DocumentRoute, Instant>,
    /// Why each document a listing offers did not mount, as it was told last
    /// ([`listing`]): an agent lists at every step it takes, so a cause is
    /// logged once, not once per listing.
    unmounted: HashMap<DocumentRoute, String>,
    /// How long this thread waits, once it served a write, for the server to
    /// hold it before answering the caller pending: the pace of the saves it
    /// served recently ([`publish::Worker::saving`]), so writes whose saves
    /// are slower than the grace do not hold the thread that serves every
    /// workspace of its share.
    save_grace: Duration,
    /// Writes served since the pace was last measured with the full grace.
    save_probes: u32,
    /// Whether documents search keeps no entries for wait for the index's own thread to read
    /// them, or a workspace's folder for the pass over it this thread takes
    /// ([`Worker::warm_entries`], [`Worker::warm_files`]).
    warming: bool,
    /// The thread that reads documents for the search index, off this one: a document of a
    /// notebook of a hundred notes costs seconds to read for search, and every request of this
    /// share's workspaces waited for it while it was read here.
    warm: index::Warm,
    /// The documents the request being served wrote to ([`publish::Worker::saved`]):
    /// what its answer's link opens ([`share::Worker::changed_link`]).
    wrote: HashSet<DocumentRoute>,
    pages: pages::Pages,
}

/// The API's answer for an action it has nothing by that address.
fn unknown_action() -> Outcome {
    Outcome::Rejected(ActionCallError::UnknownAction)
}

/// Whether `caller` is `person`, or an agent a device of theirs runs under a key they
/// certified for it.
fn acts_for(caller: &Caller, person: &str) -> bool {
    if let Some(id) = caller.acting().person_id() {
        return id == person;
    }
    caller.is_device_agent()
        && caller.acting().acted_for_person() == Some(person)
        && caller.user_public_key().map(hex::encode).as_deref() == Some(person)
}

/// What a held record publishes that its API runs now.
fn live_actions(app: &ConfinedReplica) -> Vec<tilo_actions::ActionDescriptor> {
    app.survey()
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
    let taking = timing_start();
    let own = app.take_mutation()?;
    timing_done("take.own", taking);
    let taking = timing_start();
    let beside = match app.take_document_mutations() {
        Ok(beside) => beside,
        Err(error) => {
            if let Some(own) = own {
                app.put_back(own);
            }
            return Err(error);
        }
    };
    timing_done("take.beside", taking);
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

impl Worker {
    fn new(
        link: &dyn WorkspaceDocumentsLink,
        catalogue: AppCatalogue,
        shared: Arc<Shared>,
    ) -> Self {
        let own_device = link.own_device();
        let _ = shared.own_device.set(own_device.clone());
        Self {
            member: ui::permissions::policy::PersonId::from_root(link.user_public_key()),
            own_device,
            catalogue,
            shared,
            workspaces: HashMap::new(),
            mounted: BTreeMap::new(),
            bounded: BTreeMap::new(),
            parked: bounded::Parked::default(),
            beside: HashMap::new(),
            channels_opened: HashSet::new(),
            titles: crate::worker::ConfinedTitles::default(),
            said_directly: HashMap::new(),
            joining: HashMap::new(),
            part_waits: part_wait::PartWaits::default(),
            call_waits: call_wait::CallWaits::default(),
            agent_read_answered: false,
            part_loads: part_load::PartLoads::default(),
            writing_as: None,
            pages: pages::Pages::default(),
            clock: 0,
            handed: HashMap::new(),
            canonical: canonical::CanonicalReads::default(),
            wrote: HashSet::new(),
            named: HashMap::new(),
            unmounted: HashMap::new(),
            save_grace: publish::SAVE_GRACE,
            save_probes: 0,
            warming: false,
            warm: index::Warm::start(),
        }
    }

    fn run(mut self, jobs: &mpsc::Receiver<(Instant, Job)>) {
        // What came and waits for this thread: a person's first, then an agent's, each most
        // urgent first ([`queue`]).
        let mut queue = queue::Queue::default();
        loop {
            // What the index's own thread read since the last pass is taken here, and the
            // next documents asked for; the folder's chunks are read here, between requests.
            // A find answers from the indexes, never by opening every one while the person
            // waits, and no request waits on a document read.
            if self.warming {
                let entries = self.warm_entries();
                let files = self.warm_files(index::WARM_SLICE);
                self.warming = !(entries && files);
            }
            queue.take_sent(jobs, &self.shared.log);
            self.try_parts_again(None);
            if std::mem::take(&mut self.agent_read_answered) {
                queue.agent_read_answered();
            }
            if let Some(queue::Waiting { queued, job, from }) = queue.next() {
                self.expire_watchers();
                match from {
                    queue::Origin::Sent => self.admit(queued, job),
                    queue::Origin::Returned(tried) => self.serve_returned(queued, job, tried),
                }
                queue.put_back(self.returned_to_queue(), &self.shared.log);
                // Requests that keep coming never hold back a run's word:
                // the page forgets a member it has not heard from.
                if session_voice::next_due(&self.workspaces)
                    .is_some_and(|due| due <= Instant::now())
                {
                    self.tick_voices();
                }
                continue;
            }
            if queue.closed() && self.part_waits.is_empty() {
                break;
            }
            let due = [
                self.joining_until(),
                self.part_waits.until(),
                session_voice::next_due(&self.workspaces),
                self.watchers_until(),
                // An agent's request waits a moment for the person's next one.
                queue.paused_until(),
            ]
            .into_iter()
            .flatten()
            .min();
            let next = match due {
                // With documents still to read for the index, the thread looks for what was
                // read and for the requests that came: a request queued is taken at once, and
                // the index is ready as soon as the workspace is.
                _ if self.warming => jobs.recv_timeout(index::WARM_POLL),
                Some(until) => jobs.recv_timeout(until.saturating_duration_since(Instant::now())),
                None => jobs
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            self.expire_watchers();
            match next {
                Ok(job) => queue.put(job, &self.shared.log),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.give_up_joining();
                    self.tick_voices();
                    self.expire_watchers();
                }
                // What waits is still served, then the thread ends.
                Err(mpsc::RecvTimeoutError::Disconnected) => queue.close(),
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
        self.take_up(queued, job);
        // What landed while the job read its workspace wakes who waits for a change of it.
        self.wake_watchers();
    }

    /// [`Self::admit`] without the look at who waits for a change.
    fn take_up(&mut self, queued: Instant, job: Job) {
        // A workspace's change woke this thread to run what waited for it.
        // Nothing waits usually, and most wakes then cost nothing: reading
        // the workspace for each wake would make a busy workspace pay a read
        // for every change the service hears, including the changes of every
        // other workspace.
        if matches!(job, Job::Joined(_) | Job::Called(_) | Job::Loaded(_)) {
            self.timed(queued, job);
            return;
        }
        if self.warm.in_flight() {
            self.shared
                .search
                .beside_reading
                .fetch_add(1, Ordering::Relaxed);
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
            Job::Page(page) => self.page_keys(page),
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
        joining::hold(
            self,
            workspace_id,
            Waiting {
                queued,
                deadline,
                job,
            },
        );
        // A write waiting for its part looks again: the change may be the part.
        self.try_parts_again(Some(workspace_id));
    }

    /// Invocation the requests waiting for `workspace_id`, its change just arrived:
    /// those that are ready run here, and those still waiting for keys are
    /// held again with what they have waited so far.
    fn resume(&mut self, workspace_id: WorkspaceId) {
        joining::resume(self, workspace_id);
        // A write waiting for its part looks again: the change may be the part.
        self.try_parts_again(Some(workspace_id));
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

    /// Stop asking to be woken for `workspace_id` once nothing waits for it: no request held
    /// for its join, its keys or a part it changes, and nobody waiting for a change of it.
    fn stop_waiting_if_idle(&self, workspace_id: WorkspaceId) {
        joining::stop_waking_if_idle(self, workspace_id);
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
    /// that waited or ran long at warn. A catalogue page names the resource
    /// it listed under and the one it looked for, so a slow lookup says which
    /// of the resource's levels was read.
    fn timed(&mut self, queued: Instant, job: Job) {
        // An operation of its workspace goes on off the thread, and it reads what that changes.
        let Some(job) = self.unless_behind_call(queued, job) else {
            return;
        };
        let started = Instant::now();
        let kind = job.kind();
        let workspace = job.workspace().map(|id| id.to_string()).unwrap_or_default();
        let (parent, named) = match &job {
            Job::Catalogue { query, .. } => (
                query.parent_resource.clone().unwrap_or_default(),
                query.resource_name.clone().unwrap_or_default(),
            ),
            _ => (String::new(), String::new()),
        };
        let step = job.step_kind();
        self.part_waits.begin(
            queued,
            match job {
                Job::Catalogue { .. } => part_wait::Serving::Listing,
                _ if job.holds_for_parts() => part_wait::Serving::Write,
                _ => part_wait::Serving::Other,
            },
        );
        // One applied again after waiting off this thread, for parts or for a call's answer,
        // is the request it was, not one more.
        let again = self.part_waits.again() || self.call_waits.taking_an_answer();
        let agent_read = queue::agent_read(&job, &self.shared.log);
        self.call_waits.begin();
        self.take(queued, job);
        // Answered, not held for a call or a part: the listing it is a page of reads its next
        // page before the agents' next write starts.
        if agent_read && !self.call_waits.held() && !self.part_waits.held() {
            self.agent_read_answered = true;
        }
        // What it ended, it ended now, as a listing of runs says.
        self.log().stamp_ended();
        let (waited, ran) = (started.duration_since(queued), started.elapsed());
        tracing::debug!(
            kind,
            workspace,
            parent,
            named,
            again,
            waited_ms = waited.as_millis(),
            ran_ms = ran.as_millis(),
            "workspace actions: a request ran"
        );
        if waited.max(ran) >= SLOW_JOB {
            tracing::warn!(
                kind,
                workspace,
                parent,
                named,
                step = step.unwrap_or("-"),
                waited_ms = waited.as_millis(),
                ran_ms = ran.as_millis(),
                "workspace actions: a request waited or ran long"
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per kind of request, read together: a dozen tiny methods hide the map"
    )]
    fn take(&mut self, queued: Instant, job: Job) {
        // What it renders for its caller shows their dates and clocks as their own windows do.
        let _region = crate::worker::region::scope(self.region_for(&job));
        {
            match job {
                Job::Feedback {
                    workspace_id,
                    query,
                    caller,
                    done,
                } => match self.shared.link.get().cloned() {
                    Some(link) => {
                        // It proves the destination through the resource's registered read, and
                        // projects its report from the committed source in a worker: that goes
                        // on off this thread once it outlasts a call, and answers the caller
                        // from there.
                        let catalogue = self.catalogue.clone();
                        let holder = DocumentRoute {
                            app_id: "so.tilo.chat".into(),
                            cluster_id: workspace_id.to_string(),
                            document: String::new(),
                        };
                        let asked = self.hand(workspace_id, &holder, move || {
                            feedback::answer_and_review(
                                &link,
                                catalogue,
                                workspace_id,
                                caller,
                                *query,
                                done,
                            );
                        });
                        if asked.is_none() {
                            self.await_completed(workspace_id);
                        }
                    }
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
                    if !self.part_waits.again() {
                        self.voice_catalogue(workspace_id, &caller, &query);
                    }
                    let result = if query.view_resource.is_some() {
                        self.view_catalogue_page(workspace_id, &caller, &query)
                    } else if query.provider.as_deref()
                        == Some(tilo_actions::remote::workspace_invocations::PROVIDER)
                    {
                        self.invocations_page(workspace_id, &caller, &query)
                    } else {
                        self.catalogue_page(workspace_id, &caller, &query)
                    };
                    // Its app's worker answers it off this thread, or the parts of the chains
                    // it lists are on their way: it waits for either there, and every other
                    // request of the workspace is served meanwhile.
                    if self.call_waits.held() {
                        self.await_call(
                            workspace_id,
                            queued,
                            Job::Catalogue {
                                workspace_id,
                                query,
                                caller,
                                done,
                            },
                        );
                    } else if self.hold_for_chains() || self.part_waits.held() {
                        self.await_part(
                            workspace_id,
                            Job::Catalogue {
                                workspace_id,
                                query,
                                caller,
                                done,
                            },
                        );
                    } else {
                        done.complete(result);
                    }
                }
                Job::Invoke {
                    workspace_id,
                    invocation,
                    done,
                } => match self.invoke(workspace_id, invocation) {
                    Some(receipt) => done.complete(receipt),
                    // Its app's worker answers it off this thread.
                    None if self.call_waits.held() => self.await_call(
                        workspace_id,
                        queued,
                        Job::Invoke {
                            workspace_id,
                            invocation,
                            done,
                        },
                    ),
                    // It waits for a part of what it changes, off this thread.
                    None => self.await_part(
                        workspace_id,
                        Job::Invoke {
                            workspace_id,
                            invocation,
                            done,
                        },
                    ),
                },
                Job::Ran(ended) => self.ran(&ended),
                Job::Opened(ended) => self.settle_open(&ended),
                Job::Typed(ended) => self.typed(&ended),
                Job::Arrived {
                    workspace_id,
                    caller,
                } => self.arrived(workspace_id, &caller),
                Job::ReadDocument {
                    workspace_id,
                    caller,
                    address,
                    done,
                } => done.complete(self.document_read(workspace_id, &caller, &address)),
                Job::Search {
                    workspace_id,
                    caller,
                    needle,
                    within,
                    done,
                } => done.complete(self.asking_here(|worker| {
                    worker.search(workspace_id, &caller, (&needle, &within))
                })),
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
                Job::Called(workspace_id) => self.call_answered(workspace_id),
                Job::Loaded(workspace_id) => self.parts_loaded(workspace_id),
                Job::Rename(workspace_id, route) => self.rename(workspace_id, &route),
                Job::Landed(workspace_id, route) => {
                    if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
                        workspace.landed_elsewhere(route);
                    }
                }
                Job::Page(job) => self.page_job(*job),
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

    /// The region the views `job` renders are in: that of the person its caller is or acts
    /// for, where this device keeps one for them ([`WorkspaceDocumentsLink::region_of`]).
    fn region_for(&self, job: &Job) -> Option<PersonRegion> {
        let link = self.shared.link.get()?;
        let caller = match job {
            Job::Invoke { invocation, .. } => self
                .log()
                .entries
                .get(invocation)
                .map(|entry| entry.caller.clone()),
            Job::Page(page) => self.pages.caller_of(page).cloned(),
            other => other.caller().cloned(),
        }?;
        link.region_of(&caller)
    }

    fn log(&self) -> std::sync::MutexGuard<'_, InvocationLog> {
        self.shared.log.lock_recover()
    }

    /// Let go of everything held of `workspace_id`, deleted: its
    /// membership, the records mounted and the replicas callers acted
    /// through there, its file tree, and its content store. Requests still waiting for it
    /// run, and are told it is not there.
    fn forget(&mut self, workspace_id: WorkspaceId) {
        let of =
            |route: &DocumentRoute| WorkspaceId::parse(&route.cluster_id) == Some(workspace_id);
        self.workspaces.remove(&workspace_id);
        self.shared.joins.lock_recover().remove(&workspace_id);
        self.shared.stores.lock_recover().remove(&workspace_id);
        self.mounted.retain(|route, _| !of(route));
        self.bounded.retain(|(route, ..), _| !of(route));
        self.parked.retain(|(route, ..)| !of(route));
        self.beside.retain(|route, held| !of(route) && !of(held));
        self.channels_opened.retain(|route| !of(route));
        self.canonical.forget(of);
        for Waiting { queued, job, .. } in self.joining.remove(&workspace_id).unwrap_or_default() {
            self.timed(queued, job);
        }
        self.try_parts_again(Some(workspace_id));
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
                None => Workspace::join(&self.own_device, workspace_id, &self.shared)
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
        let waited = timing_start();
        workspace.wait_for(
            workspace_id,
            needs,
            self.shared.link.get().map(|link| &**link),
            !self.part_waits.answer_only(),
        )?;
        timing_done("refresh.wait", waited);
        let stating = timing_start();
        let state = workspace.state();
        timing_done("refresh.state", stating);
        // Only what a write landed in since the last refresh is merged: what
        // is held mounted is kept current by its changes, never by reading
        // every part the app keeps again.
        // What is held mounted takes it up when it is next used: a write by
        // one caller is merged into no one else's replica on its way in. A
        // caller's own replica catches up as it is taken
        // ([`Self::bounded_replica`]).
        let changed = workspace.take_changed();
        let held = self
            .mounted
            .values_mut()
            .chain(self.bounded.values_mut().map(|replica| &mut replica.held));
        for mounted in held {
            mounted
                .behind
                .get_or_insert_with(HashSet::new)
                .extend(changed.iter().cloned());
        }
        // A write that landed since the last pass is read for the index as soon as this
        // request is answered, so the next find answers from it.
        let indexing = timing_start();
        let files_waiting = self.files_unwarmed_any();
        if !self.warming && (self.unindexed_any() || files_waiting) {
            self.warming = true;
        }
        timing_done("refresh.index", indexing);
        Ok(state)
    }

    /// Records of the workspace an app of the catalogue opens: those on the
    /// shared desktop first, then the most recently changed.
    /// The actions `route` lists: someone served within bounds is listed
    /// what their own replica offers them, what the host runs for them and
    /// nothing else; the owner, what the mounted resource offers. An app this
    /// device only has a package of is listed from the surface the package
    /// carries, as the actions of an app compiled in are listed from the app.
    fn listed(
        &mut self,
        workspace_id: WorkspaceId,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, String> {
        if self.catalogue.get(&route.app_id).is_none() {
            return self.packaged_descriptors(workspace_id, bounded, route);
        }
        if let Some(who) = bounded {
            self.bounded_actions(workspace_id, who, route, state)
                .map_err(|error| error.to_string())
        } else {
            // Its app's worker answers it: read off this thread once it outlasts a call.
            let app = Arc::clone(&self.mount(route, state)?.app);
            let holder = self.holder_of(route);
            self.hand(workspace_id, &holder, move || live_actions(&app))
                .ok_or_else(|| "the app's worker is still answering".to_owned())
        }
    }

    /// The actions the version of `route`'s app the workspace installs
    /// declares, as the surface its package carries lists them: what a caller
    /// is offered, and never a row the caller's own access refuses. `Err` for
    /// an app whose package this device does not hold.
    fn packaged_descriptors(
        &self,
        workspace_id: WorkspaceId,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, String> {
        let surface = self
            .packaged_surface(workspace_id, &route.app_id)
            .ok_or_else(|| format!("{} installs no package this device holds", route.app_id))?;
        let permissions = match bounded {
            Some(who) => Some(
                self.permissions_at(workspace_id, who, route)
                    .ok_or_else(|| format!("{} is not admitted to this resource", route.app_id))?,
            ),
            None => None,
        };
        Ok(packaged::descriptors(&surface)
            .into_iter()
            .filter(|descriptor| {
                permissions.as_ref().is_none_or(|permissions| {
                    permissions.answer(&TreePath::root(), descriptor.contract.kind) != Answer::Deny
                })
            })
            .collect())
    }

    /// The records of `cluster_id` a listing shows: those this member's
    /// replica holds, and those the workspace's host holds of an app the
    /// catalogue has or one this device holds the installed package of.
    ///
    /// A public workspace sends a device its records only when it opens one,
    /// so the replica holds a handful of them: without the host's list, every
    /// app there is installed and none running, and what a member's agent
    /// names is not found. A record listed here that the replica has not been
    /// sent is asked for when a request names it.
    fn documents(
        &mut self,
        workspace_id: Option<WorkspaceId>,
        state: &WorkspaceState,
        cluster_id: &str,
    ) -> Vec<DocumentRoute> {
        let mut routes: Vec<DocumentRoute> = state.documents.to_vec();
        // What this member's replica holds, before the serving device's list adds the
        // documents it has not been sent.
        let kept_here: HashSet<DocumentRoute> = routes.iter().cloned().collect();
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
        // What people have open on the shared desktop first, then the most
        // recently changed: read here, where a listing is answered, and not
        // by every request that refreshes the workspace.
        // Another person's own document: the workspace sends it to that person's devices
        // alone, so this service never holds it. Listed, it was the app's newest document,
        // and every path an agent named in the app was read through it first, waiting out a
        // listing that could never answer before the one the person's window shows was read.
        if let Some(workspace) =
            WorkspaceId::parse(cluster_id).and_then(|id| self.workspaces.get(&id))
        {
            routes.retain(|route| !workspace.not_yours.contains(route));
        }
        let routes = match WorkspaceId::parse(cluster_id) {
            Some(id) => match self.workspaces.get_mut(&id).and_then(|workspace| {
                workspace
                    .documents
                    .as_mut()
                    .map(|kept| kept.people_first_of(routes.clone()))
            }) {
                Some(ordered) => ordered,
                None => routes,
            },
            None => routes,
        };
        routes
            .into_iter()
            .filter(|route| {
                self.catalogue
                    .get(&route.app_id)
                    .is_some_and(|app| opens_as_its_own(app, &route.document))
                    // An app this device has only a package of keeps its
                    // where every holder of the workspace sees it: the
                    // package says what it publishes, never where it is kept.
                    || (workspace_id.is_some_and(|workspace_id| {
                        self.installed_package(workspace_id, &route.app_id).is_some()
                    }) && !is_part(&route.document))
            })
            // What this service holds as a document: one another device made and this one
            // keeps only as retained changes (a window's own document, a sealed one it cannot
            // open) is no data of the app here, and is neither listed nor offered. A document
            // the replica holds none of is not one it keeps that way: the workspace may still
            // send it, so it is listed and asked for when a request names it. A part of a
            // chain is always kept: a public workspace sends one to this device when its
            // chain is asked for, and the listing is what asks.
            .filter(|route| {
                is_part(&route.document)
                    || !kept_here.contains(route)
                    || state
                        .reads
                        .record(&DocumentRoute {
                            app_id: route.app_id.clone(),
                            cluster_id: cluster_id.to_owned(),
                            document: route.document.clone(),
                        })
                        .ok()
                        .flatten()
                        .is_some()
            })
            .collect()
    }

    fn view_catalogue_page(
        &mut self,
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
        // confined worker, never here. Reading it outlasts a slow call: it goes on off this
        // thread, and this request takes its answer when it is applied again.
        let revision = source.revision;
        let read = {
            let (wanted, first) = (wanted.clone(), first.clone());
            #[cfg(feature = "test-support")]
            let route = route.clone();
            move || -> Result<(Vec<_>, Vec<_>), ActionCallError> {
                #[cfg(feature = "test-support")]
                crate::workspace_actions::slowed::wait(&route.cluster_id, &route.app_id);
                let mut views = ConfinedViews::load(app_id, source.bytes)
                    .map_err(|reason| ActionCallError::Unavailable { reason })?;
                let descriptors = views.views(&wanted, &first).map_err(|_| refuse())?;
                let current = views.views(&wanted, &first).map_err(|_| refuse())?;
                Ok((descriptors, current))
            }
        };
        let Some(read) = self.hand(workspace, &route, read) else {
            return Err(ActionCallError::Unavailable {
                reason: "the app's worker is still answering".into(),
            });
        };
        let (descriptors, current) = read?;
        // The policy the worker read under stands as it was: one that moved while the worker
        // answered is refused, as one the worker was told of would be.
        let fresh = link
            .view_read_policy(workspace, caller, &route.app_id, &route.document)
            .ok_or_else(refuse)?;
        if fresh.workspace != first.workspace
            || fresh.access != first.access
            || fresh.sandbox != first.sandbox
        {
            return Err(refuse());
        }
        let rows = descriptors
            .into_iter()
            .filter(|descriptor| current.contains(descriptor))
            .map(|descriptor| {
                CatalogueRow::for_action(provider.to_owned(), route.app_id.clone(), descriptor)
            })
            .collect();
        let mut page = CataloguePage::from_rows(revision, rows, query);
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
        &mut self,
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
        // The read is mounted in a worker, off this thread once the call outlasts one.
        let prepared = {
            let (catalogue, link, caller, asked) = (
                self.catalogue.clone(),
                link.clone(),
                caller.clone(),
                route.clone(),
            );
            let mut hand =
                call_wait::Handing::of(&mut self.call_waits, &mut self.part_waits, &self.shared);
            hand.hand(workspace, &route, move || {
                let Ok(mut read) = reads::CanonicalRead::prepare(
                    &catalogue,
                    link.as_ref(),
                    workspace,
                    &caller,
                    &asked,
                ) else {
                    return Ok::<_, String>(None);
                };
                let descriptors = read.operations();
                read.recheck(link.as_ref(), workspace, &caller, &asked)?;
                Ok(Some((read, descriptors)))
            })
        };
        // `None` for a worker still answering: the request waits off this thread for it.
        let prepared = prepared?;
        let Ok(Some((read, descriptors))) = prepared else {
            return None;
        };
        if !descriptors.iter().any(|entry| &entry.address == target) {
            return None;
        }
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
}

/// The service's thread holds a workspace's requests until it is ready for them ([`joining`]).
impl joining::Holder for Worker {
    type Job = Job;

    fn joining(&self) -> &HashMap<WorkspaceId, VecDeque<Waiting>> {
        &self.joining
    }

    fn joining_mut(&mut self) -> &mut HashMap<WorkspaceId, VecDeque<Waiting>> {
        &mut self.joining
    }

    fn wake_on_change(&self, workspace_id: WorkspaceId, waiting: bool) {
        self.awaiting(workspace_id, waiting);
    }

    fn ready_for(&mut self, workspace_id: WorkspaceId, job: &Job) -> bool {
        let keys = self.keys(job);
        job.workspace() != Some(workspace_id) || self.ready(workspace_id, &keys)
    }

    fn run(&mut self, queued: Instant, job: Job) {
        self.timed(queued, job);
    }

    /// A write waiting for a part it changes, or someone waiting for a change of it.
    fn waits_besides(&self, workspace_id: WorkspaceId) -> bool {
        self.part_waits.any_in(workspace_id)
            || self
                .workspaces
                .get(&workspace_id)
                .is_some_and(|workspace| workspace.watchers.any())
    }
}

#[cfg(test)]
mod tests;

use job::Job;
