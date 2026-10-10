//! Dynamic provider backed by the resource/action surfaces live Tilo apps
//! publish through the session collaboration broker.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_search::{Document, Matching, Page, SearchIndex};
use ui::search::FieldWeight;

use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::ActionReceipt;
use crate::shell::search::action_api::{ActionError, ActionHierarchy, ActionParam, ParamType};
use crate::shell::search::{
    InvocationDisposition, PendingInvocation, ResourceKind, SearchBudget, SearchContext,
    SearchCursor, SearchPage, SearchProvider, SearchProximity, SearchResult,
};

use crate::shell::collab_broker_capnp::{
    ActionDirectoryRoot, PublishedAction, PublishedInput, PublishedSurface, launchable_provider,
};
use tilo_actions::remote::{
    ActionHostError, CatalogueLimits, CataloguePage, CatalogueQuery, CatalogueRow, Confirmation,
    InvocationReceipt, InvokeArgument, InvokeRequest, Pending, PendingPoll, RemoteActionTarget,
};

pub const PROVIDER_ID: &str = "app-actions";
/// How long the palette waits for a local action's admission. A closed app
/// is launched first, and the host gives up on it sooner with a clearer
/// error; this only bounds a host that never answers.
const LOCAL_ADMISSION_TIMEOUT: Duration = Duration::from_secs(40);
const ID_SEPARATOR: char = '\u{1f}';
const MAX_DIRECTORY_ROOTS_PER_QUERY: usize = 16;
/// Jobs are an immediate, useful palette hint rather than a second unbounded
/// search corpus.  They are deliberately a small first stage before remote
/// and local actions.
const MAX_JOB_ROWS_PER_QUERY: usize = 4;
/// Directory responses are an acceleration cache, not a second unbounded
/// replica of every connected application's action namespace.
const MAX_RETAINED_DIRECTORY_PAGES: usize = 64;
const MAX_RETAINED_SERVICE_PAGES: usize = 16;
const SERVICE_DIAGNOSTIC_TARGET: &str = "tilo_native_governance";
/// A stalled LocalSet/capability must not leave a palette-owned oneshot alive
/// forever. Retrying after this deadline remains bounded by the bridge queue.
const DIRECTORY_PAGE_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

fn report_certified_job_refusal(outcome: &'static str) {
    tracing::debug!(
        target: SERVICE_DIAGNOSTIC_TARGET,
        stage = "certified_job_status",
        outcome,
        "native certified job observation refused"
    );
}

#[cfg(test)]
thread_local! {
    static TEST_DIRECTORY_START_TIMEOUT: Cell<Option<Duration>> = const { Cell::new(None) };
}

fn directory_start_timeout() -> Duration {
    #[cfg(test)]
    if let Some(timeout) = TEST_DIRECTORY_START_TIMEOUT.with(Cell::get) {
        return timeout;
    }
    DIRECTORY_PAGE_REPLY_TIMEOUT
}

#[cfg(test)]
pub(crate) fn set_directory_start_timeout_for_test(timeout: Option<Duration>) {
    TEST_DIRECTORY_START_TIMEOUT.with(|slot| slot.set(timeout));
}

/// One remote directory page is addressed by its certified route, actor,
/// query, and opaque remote continuation. Keeping the continuation in the
/// key lets a bounded palette walk a long directory without replacing an
/// already-rendered page.
type DirectoryCacheKey = (u64, String, String, Vec<u8>);

/// One eager broker catalogue for the certified Person in a workspace.
/// Unlike application directories, native workspace services use opaque
/// provider names (for example `workspace:<room>`) rather than peer ids.
type ServiceCacheKey = (String, String, String);

struct PendingDirectoryPage {
    receiver: Pending<Result<CataloguePage, ActionHostError>>,
    deadline: Instant,
    /// The first page changes the visible source and stales old cursors. A
    /// later page merely appends behind an existing continuation, which keeps
    /// that continuation valid and avoids replaying earlier remote rows.
    initial_page: bool,
}

/// Whether a refusal means the cached page no longer describes the
/// directory: the row went away, its revision moved on, or the owner did.
fn invalidates_directory_cache(error: &ActionHostError) -> bool {
    matches!(
        error,
        ActionHostError::UnknownAction
            | ActionHostError::InvalidRequest { .. }
            | ActionHostError::Unavailable { .. }
    )
}

/// The parameters a caller must also fill for `policy`: an explicit
/// confirmation or the typed confirmation phrase.
fn push_confirmation_params(
    params: &mut Vec<ActionParam>,
    policy: &tilo_actions::ConfirmationPolicy,
) {
    match policy {
        tilo_actions::ConfirmationPolicy::None => {}
        tilo_actions::ConfirmationPolicy::Confirm => params.push(ActionParam::required(
            "confirm",
            ParamType::Boolean,
            "Explicitly confirm this consequential action",
        )),
        tilo_actions::ConfirmationPolicy::Phrase { .. } => params.push(ActionParam::required(
            "confirmation_phrase",
            ParamType::Text,
            "Type the action's confirmation phrase exactly",
        )),
    }
}

fn directory_start_ticket(
    admission: Pending<Result<(u64, InvocationReceipt), ActionHostError>>,
    invalidations: Rc<RefCell<HashSet<DirectoryCacheKey>>>,
    cache_key: DirectoryCacheKey,
    deadline: Instant,
) -> PendingInvocation {
    let refused_invalidations = Rc::clone(&invalidations);
    let refused_key = cache_key.clone();
    admission_ticket(
        admission,
        deadline,
        move |error| {
            if invalidates_directory_cache(error) {
                refused_invalidations
                    .borrow_mut()
                    .insert(refused_key.clone());
            }
            ActionError::Refused(format!("directory action refused: {error}"))
        },
        move || {
            invalidations.borrow_mut().insert(cache_key.clone());
            ActionError::Refused("directory action admission timed out".into())
        },
    )
}

/// Poll an admission from the frame loop until it answers or `deadline`
/// passes. Dropped once answered or given up on, so a start still waiting
/// sees nobody wants it and does not begin after the deadline.
///
/// The answer arrives on the broker's thread; it wakes the loop that made
/// the ticket ([`invocation_wake`](crate::shell::search::invocation_wake)),
/// so an idle desktop polls it again at once instead of on its next
/// unrelated wake.
fn admission_ticket(
    admission: Pending<Result<(u64, InvocationReceipt), ActionHostError>>,
    deadline: Instant,
    refused: impl Fn(&ActionHostError) -> ActionError + 'static,
    timed_out: impl Fn() -> ActionError + 'static,
) -> PendingInvocation {
    let admission = Rc::new(RefCell::new(Some(admission)));
    let waker = crate::shell::search::invocation_wake::waker();
    Rc::new(move || {
        let polled = admission
            .borrow_mut()
            .as_mut()
            .map_or(PendingPoll::Abandoned, |admission| {
                crate::shell::search::invocation_wake::poll(admission, &waker)
            });
        if !matches!(polled, PendingPoll::Waiting) || Instant::now() >= deadline {
            admission.borrow_mut().take();
        }
        match polled {
            PendingPoll::Ready(Ok((job, receipt))) => Ok(Some(ActionReceipt {
                broker_invocation_id: Some(job),
                update: receipt_update(receipt),
            })),
            PendingPoll::Ready(Err(error)) => Err(refused(&error)),
            PendingPoll::Abandoned => Err(ActionError::Refused(
                "the action's admission was unavailable".into(),
            )),
            PendingPoll::Waiting if Instant::now() >= deadline => Err(timed_out()),
            PendingPoll::Waiting => Ok(None),
        }
    })
}

fn receipt_update(receipt: InvocationReceipt) -> tilo_actions::InvocationUpdate {
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: receipt.invocation,
                output,
            }
        }
        tilo_actions::remote::ReceiptStatus::Failed { error } => {
            tilo_actions::InvocationUpdate::Failed {
                invocation: receipt.invocation,
                error,
            }
        }
        tilo_actions::remote::ReceiptStatus::Cancelled { message } => {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: receipt.invocation,
                message,
            }
        }
        tilo_actions::remote::ReceiptStatus::Accepted
        | tilo_actions::remote::ReceiptStatus::Rejected { .. } => {
            tilo_actions::InvocationUpdate::Accepted {
                invocation: receipt.invocation,
            }
        }
    }
}

pub(crate) const JOB_UNAVAILABLE_MESSAGE: &str = "invocation is unavailable or not visible; this does not establish the original action's outcome";

pub(crate) fn job_control_target(id: &str) -> Option<(u64, bool)> {
    let (kind, number) = id.split_once(ID_SEPARATOR)?;
    let cancel = match kind {
        "job" => false,
        "job-cancel" => true,
        _ => return None,
    };
    Some((number.parse().ok()?, cancel))
}

fn job_state(update: &tilo_actions::InvocationUpdate) -> &'static str {
    match update {
        tilo_actions::InvocationUpdate::Accepted { .. } => "accepted",
        tilo_actions::InvocationUpdate::Progress { .. } => "running",
        tilo_actions::InvocationUpdate::Item { .. } => "streaming",
        tilo_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
        tilo_actions::InvocationUpdate::Failed { .. } => "failed",
        tilo_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
    }
}

/// A [`job_state`] as the job's row reads it, in the desktop's language.
fn job_state_label(state: &str) -> String {
    match state {
        "accepted" => t("search-app-job-accepted"),
        "running" => t("search-app-job-running"),
        "streaming" => t("search-app-job-streaming"),
        "succeeded" => t("search-app-job-succeeded"),
        "failed" => t("search-app-job-failed"),
        _ => t("search-app-job-cancelled"),
    }
}

pub fn job_status_id(id: u64) -> String {
    format!("job{ID_SEPARATOR}{id}")
}

pub fn job_cancel_id(id: u64) -> String {
    format!("job-cancel{ID_SEPARATOR}{id}")
}

pub struct LiveAppActionsProvider {
    broker: AppActionBroker,
    action_sessions: ActionSessionSource,
    directory_pages: RefCell<HashMap<DirectoryCacheKey, CataloguePage>>,
    directory_pending: RefCell<HashMap<DirectoryCacheKey, PendingDirectoryPage>>,
    service_pages: RefCell<HashMap<ServiceCacheKey, CataloguePage>>,
    service_pending: RefCell<HashMap<ServiceCacheKey, PendingDirectoryPage>>,
    service_context: RefCell<Option<(String, String, u64, u64)>>,
    service_source_revision: RefCell<Option<String>>,
    /// One bit per closed observation, bounded by the same service-page keys.
    /// Repeated headless catalogue surveys must not flood the production log.
    service_diagnostics: RefCell<HashMap<ServiceCacheKey, u16>>,
    /// The palette owns at most one needle's directory projection.
    /// Keeping previous query terms made rapid typing retain unbounded pages
    /// and receivers even though no result could ever be rendered again.
    directory_query: RefCell<Option<String>>,
    resource_kinds: Vec<ResourceKind>,
    directory_invalidations: Rc<RefCell<HashSet<DirectoryCacheKey>>>,
    service_invalidations: Rc<RefCell<HashSet<ServiceCacheKey>>>,
    directory_epoch: AtomicU64,
    surface_snapshot: RefCell<Option<SurfaceSnapshot>>,
    surface_build: RefCell<Option<Receiver<SurfaceSnapshot>>>,
    /// The long-lived index worker, started by the first rebuild.
    index_worker: RefCell<Option<mpsc::Sender<IndexJob>>>,
    auxiliary_snapshot: RefCell<Option<AuxiliarySnapshot>>,
    projection: Cell<QueryProjection>,
    #[cfg(test)]
    build_probe: Arc<BuildProbe>,
    #[cfg(test)]
    auxiliary_service_probe: RefCell<Option<Box<dyn FnOnce()>>>,
    #[cfg(test)]
    directory_query_probe: RefCell<Option<CatalogueQuery>>,
}

struct SurfaceSnapshot {
    source_generation: u64,
    scope: String,
    entries: Vec<(PublishedSurface, PublishedAction)>,
    /// The entries' fields keyed by position, best match first. `None` for a
    /// one-row projection of an entry that already matched.
    index: Option<SearchIndex>,
    machine_index: Option<SearchIndex>,
}

/// One rebuild the index worker owes: the scope to project, where the
/// finished snapshot goes, and the loop to wake once it is there.
struct IndexJob {
    scope: SurfaceBuildScope,
    reply: mpsc::SyncSender<SurfaceSnapshot>,
    wake: std::task::Waker,
}

#[cfg(test)]
#[derive(Default)]
struct BuildProbe {
    builds: std::sync::atomic::AtomicUsize,
    thread: Mutex<Option<std::thread::ThreadId>>,
}

#[derive(Clone)]
struct SurfaceBuildScope {
    key: String,
    scope: crate::shell::chrome_state::SearchScope,
    room: Option<String>,
    app: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryProjection {
    /// The normal public entry point. It delegates to the bounded pager.
    All,
    /// Construct exactly one local row while preserving the established
    /// callback and confirmation machinery.
    LocalOnly,
}

#[allow(
    clippy::large_enum_variant,
    reason = "directory descriptors remain inline in the short, bounded auxiliary page so a cursor can materialize them without another cache lookup"
)]
enum AuxiliaryEntry {
    Job {
        id: u64,
        state: &'static str,
        cancel: bool,
        actor: String,
    },
    Directory {
        root: ActionDirectoryRoot,
        revision: String,
        descriptor: tilo_actions::ActionDescriptor,
        actor: String,
        cursor: Vec<u8>,
    },
    Service {
        room: String,
        revision: String,
        row: CatalogueRow,
        actor: String,
    },
    Resource {
        root: ActionDirectoryRoot,
        row: CatalogueRow,
    },
}

struct AuxiliarySnapshot {
    revision: String,
    query: String,
    scope: String,
    resume: AuxiliaryResume,
    entries: Vec<AuxiliaryEntry>,
    pending_directory: Vec<DirectoryContinuation>,
    next_resume: Option<AuxiliaryResume>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryContinuation {
    root: crate::shell::collab_broker_capnp::ActionDirectoryRoot,
    actor: String,
    cursor: Vec<u8>,
    row_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuxiliaryResume {
    root_after: Option<u64>,
    directory: Option<DirectoryContinuation>,
}

/// A published local action, started through the session's action host
/// confined to the room of the publishing surface: the same admission,
/// permission and confirmation path every other caller takes. An action
/// retained from a closed app names the host's launchable provider, and the
/// host launches the app before delivering it.
struct LocalStart {
    action_sessions: ActionSessionSource,
    provider: String,
    room: String,
    address: tilo_actions::ActionAddress,
    expected_person: String,
    confirmation: tilo_actions::ConfirmationPolicy,
    typed_arguments: bool,
    wants_content: bool,
}

struct ServiceStart {
    action_sessions: ActionSessionSource,
    provider: String,
    room: String,
    address: tilo_actions::ActionAddress,
    actor: String,
    revision: String,
    typed_arguments: bool,
}

type ServiceAdmission = Pending<Result<(u64, InvocationReceipt), ActionHostError>>;

impl ServiceStart {
    fn begin(
        &self,
        args: &crate::shell::search::action_api::ActionArgs,
    ) -> Result<ServiceAdmission, ActionError> {
        let argument = if self.typed_arguments {
            let mut values = args.to_json();
            values.remove("confirm");
            values.remove("confirmation_phrase");
            InvokeArgument::Json(values)
        } else {
            args.text("argument").map_or(InvokeArgument::None, |text| {
                InvokeArgument::Text(text.to_owned())
            })
        };
        let confirmation = if args.boolean("confirm") == Some(true) {
            Some(Confirmation::Confirmed)
        } else {
            args.text("confirmation_phrase")
                .map(|value| Confirmation::Phrase(value.to_owned()))
        };
        Ok(self
            .action_sessions
            .open(&self.room, Some(&self.actor))
            .map_err(ActionError::Refused)?
            .invoke_job(InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: self.provider.clone(),
                    address: self.address.clone(),
                },
                argument,
                context: None,
                confirmation,
                revision: Some(self.revision.clone()),
                fresh_binding: None,
                wait_ms: None,
            }))
    }
}

impl LocalStart {
    fn begin(
        &self,
        args: &crate::shell::search::action_api::ActionArgs,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        let argument = if self.typed_arguments {
            let mut values = args.to_json();
            values.remove("confirm");
            values.remove("confirmation_phrase");
            InvokeArgument::Json(values)
        } else {
            args.text(if self.wants_content {
                "content"
            } else {
                "argument"
            })
            .map_or(InvokeArgument::None, |text| {
                InvokeArgument::Text(text.to_owned())
            })
        };
        let confirmation = match &self.confirmation {
            tilo_actions::ConfirmationPolicy::None => None,
            tilo_actions::ConfirmationPolicy::Confirm => {
                (args.boolean("confirm") == Some(true)).then_some(Confirmation::Confirmed)
            }
            tilo_actions::ConfirmationPolicy::Phrase { .. } => args
                .text("confirmation_phrase")
                .map(|phrase| Confirmation::Phrase(phrase.to_owned())),
        };
        let session = match self
            .action_sessions
            .open(&self.room, Some(&self.expected_person))
        {
            Ok(session) => session,
            Err(error) => return Pending::ready(Err(ActionHostError::Refused { reason: error })),
        };
        session.invoke_job(InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: self.provider.clone(),
                address: self.address.clone(),
            },
            argument,
            context: None,
            confirmation,
            revision: None,
            fresh_binding: None,
            wait_ms: None,
        })
    }
}

fn deadline_after(duration: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(duration).unwrap_or(now)
}

impl LiveAppActionsProvider {
    fn directory_semantic_identity(
        &self,
        peer: u64,
        revision: &str,
        resource: &str,
        operation: &str,
    ) -> Option<tilo_actions::ResourceSemanticIdentity> {
        let pages = self.directory_pages.borrow();
        let publisher = peer.to_string();
        let mut descriptors = pages
            .iter()
            .filter(|((cached_peer, _, _, _), page)| {
                *cached_peer == peer && page.revision == revision
            })
            .flat_map(|(_, page)| &page.rows)
            .filter(|row| row.provider == publisher)
            .filter_map(|row| row.action.as_ref())
            .filter(|descriptor| {
                descriptor.address.resource == resource && descriptor.address.action == operation
            });
        let first = descriptors.next()?;
        let identity = first.semantic_identity.clone()?;
        (identity.is_valid()
            && identity.kind == first.resource_kind
            && descriptors.all(|descriptor| {
                descriptor.semantic_identity.as_ref() == Some(&identity)
                    && descriptor.resource_kind == identity.kind
            }))
        .then_some(identity)
    }

    fn wants_resource_rows(wanted: &[ResourceKind]) -> bool {
        wanted.is_empty()
            || wanted
                .iter()
                .any(|kind| matches!(kind, ResourceKind::Custom(label) if label == "Resources"))
    }

    pub fn new(broker: AppActionBroker) -> Self {
        Self {
            action_sessions: ActionSessionSource::new(broker.clone()),
            broker,
            directory_pages: RefCell::new(HashMap::new()),
            directory_pending: RefCell::new(HashMap::new()),
            service_pages: RefCell::new(HashMap::new()),
            service_pending: RefCell::new(HashMap::new()),
            service_context: RefCell::new(None),
            service_source_revision: RefCell::new(None),
            service_diagnostics: RefCell::new(HashMap::new()),
            directory_query: RefCell::new(None),
            resource_kinds: vec![
                ResourceKind::Action,
                ResourceKind::Custom("action-job".into()),
                ResourceKind::Custom("Resources".into()),
            ],
            directory_invalidations: Rc::new(RefCell::new(HashSet::new())),
            service_invalidations: Rc::new(RefCell::new(HashSet::new())),
            directory_epoch: AtomicU64::new(0),
            surface_snapshot: RefCell::new(None),
            surface_build: RefCell::new(None),
            index_worker: RefCell::new(None),
            auxiliary_snapshot: RefCell::new(None),
            projection: Cell::new(QueryProjection::All),
            #[cfg(test)]
            build_probe: Arc::new(BuildProbe::default()),
            #[cfg(test)]
            auxiliary_service_probe: RefCell::new(None),
            #[cfg(test)]
            directory_query_probe: RefCell::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_fixture_identity(broker: AppActionBroker) -> Self {
        let mut provider = Self::new(broker.clone());
        provider.action_sessions = ActionSessionSource::fixture(broker);
        provider
    }

    /// Settles on the next change to any invocation's status after this
    /// call. A control reply waiting for its job to settle sleeps on this
    /// rather than polling the broker; polled with
    /// [`invocation_wake`](crate::shell::search::invocation_wake), it wakes
    /// the loop. `None` without a session runtime to watch from.
    pub fn next_invocation_change(&self) -> Option<Pending<()>> {
        // Subscribed before the caller reads the status, so a change racing
        // that read still settles this.
        let mut statuses = self.broker.invocation_changes();
        let runtime = crate::session_runtime::runtime().ok()?;
        let (signal, change) = tilo_actions::remote::pending();
        drop(runtime.spawn(async move {
            let moved = tokio::select! {
                result = statuses.changed() => result.is_ok(),
                () = signal.abandoned() => false,
            };
            // Otherwise the completer drops unanswered, which wakes a waiter
            // still there all the same.
            if moved {
                signal.complete(());
            }
        }));
        Some(change)
    }

    /// Settles on the next change to any invocation's status or any
    /// published surface after this call: what can change a listing of
    /// these actions. A control listing waiting to settle sleeps on this.
    /// `None` without a session runtime to watch from.
    pub fn next_listing_change(&self) -> Option<Pending<()>> {
        let mut statuses = self.broker.invocation_changes();
        let mut surfaces = self.broker.surface_changes();
        let runtime = crate::session_runtime::runtime().ok()?;
        let (signal, change) = tilo_actions::remote::pending();
        drop(runtime.spawn(async move {
            let moved = tokio::select! {
                Ok(()) = statuses.changed() => true,
                Ok(()) = surfaces.changed() => true,
                () = signal.abandoned() => false,
            };
            if moved {
                signal.complete(());
            }
        }));
        Some(change)
    }

    /// The broker's invocation and publication generations: a waiter that
    /// kept them knows whether either moved since, whatever woke it.
    pub fn change_generation(&self) -> (u64, u64) {
        (
            self.broker.statuses_generation(),
            self.broker.surfaces_generation(),
        )
    }

    fn local_start(
        &self,
        surface: &PublishedSurface,
        action: &PublishedAction,
        expected_person: String,
    ) -> LocalStart {
        LocalStart {
            action_sessions: self.action_sessions.clone(),
            provider: if surface.live {
                surface.peer_id.to_string()
            } else {
                launchable_provider(surface)
            },
            room: surface.route.cluster_id.clone(),
            address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
            expected_person,
            confirmation: action.contract.advertised_confirmation(),
            typed_arguments: !action.params.is_empty(),
            wants_content: matches!(
                action.input,
                PublishedInput::Text {
                    wants_content: true,
                    ..
                }
            ),
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_published_for_test(
        &self,
        surface: &PublishedSurface,
        action: &PublishedAction,
        expected_person: String,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        let args = crate::shell::search::action_api::ActionArgs::bind_json(
            &Self::action_params(action),
            supplied,
        )
        .expect("published test arguments match the action descriptor");
        self.local_start(surface, action, expected_person)
            .begin(&args)
    }

    fn surface_scope(ctx: &SearchContext<'_>) -> String {
        let room = ctx
            .chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
            .unwrap_or_default();
        let app = ctx.chrome.active_app_id().unwrap_or_default();
        let resource = ctx
            .chrome
            .search_resource_scope
            .as_ref()
            .map_or("", |scope| scope.resource.as_str());
        format!(
            "{:?}\u{1f}{room}\u{1f}{app}\u{1f}{resource}",
            ctx.effective_scope()
        )
    }

    /// Return only indexed local actions in the current scope.  The broker
    /// inventory is cloned and indexed only after its generation changes;
    /// ordinary palette edits inspect a bounded posting instead.
    fn build_scope(ctx: &SearchContext<'_>) -> SurfaceBuildScope {
        let room = ctx
            .chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        let app = ctx.chrome.active_app_id();
        SurfaceBuildScope {
            key: Self::surface_scope(ctx),
            scope: ctx.effective_scope(),
            room,
            app,
        }
    }

    /// Rebuild a complete per-scope local index in a worker.  The worker,
    /// rather than the compositor/query caller, clones the broker's source
    /// inventory. A scope owns its own projection so a huge unrelated room
    /// cannot starve the active scope's actions.
    fn build_local_snapshot(
        broker: &AppActionBroker,
        scope: &SurfaceBuildScope,
        #[cfg(test)] probe: &BuildProbe,
    ) -> SurfaceSnapshot {
        #[cfg(test)]
        {
            probe
                .builds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *probe.thread.lock().expect("live action build thread lock") =
                Some(std::thread::current().id());
        }
        let source_generation = broker.surfaces_generation();
        let entries = broker
            .surfaces()
            .into_iter()
            .filter(|surface| match scope.scope {
                crate::shell::chrome_state::SearchScope::All => true,
                crate::shell::chrome_state::SearchScope::Workspace => {
                    scope.room.as_deref() == Some(surface.route.cluster_id.as_str())
                }
                crate::shell::chrome_state::SearchScope::ActiveApp => {
                    scope.room.as_deref() == Some(surface.route.cluster_id.as_str())
                        && scope
                            .app
                            .as_deref()
                            .is_some_and(|app| app.eq_ignore_ascii_case(&surface.app_id))
                }
            })
            .flat_map(|mut surface| {
                // Rows retain their surface metadata, but must not each
                // clone the complete action directory.  Aside from wasting
                // memory, cloning before this map turns a large source into
                // quadratic worker work.
                let actions = std::mem::take(&mut surface.actions);
                // Retain every address, including actions used by accessibility
                // and automation. Only the palette's search index hides them.
                actions
                    .into_iter()
                    .map(move |action| (surface.clone(), action))
            })
            .collect::<Vec<_>>();
        let index = Some(index_actions(&entries, false));
        let machine_index = Some(index_actions(&entries, true));
        SurfaceSnapshot {
            source_generation,
            scope: scope.key.clone(),
            entries,
            index,
            machine_index,
        }
    }

    fn refresh_local_snapshot(&self, ctx: &SearchContext<'_>) {
        let receiver = self.surface_build.borrow_mut().take();
        if let Some(receiver) = receiver {
            match receiver.try_recv() {
                Ok(snapshot) => *self.surface_snapshot.borrow_mut() = Some(snapshot),
                Err(TryRecvError::Empty) => *self.surface_build.borrow_mut() = Some(receiver),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        let generation = self.broker.surfaces_generation();
        let scope = Self::build_scope(ctx);
        if self
            .surface_snapshot
            .borrow()
            .as_ref()
            .is_none_or(|snapshot| {
                snapshot.source_generation != generation || snapshot.scope != scope.key
            })
            && self.surface_build.borrow().is_none()
        {
            let (reply, receiver) = mpsc::sync_channel(1);
            let job = IndexJob {
                scope,
                reply,
                // The finished build wakes the loop that asked for it, so a
                // listing waiting to be complete is answered when it is.
                wake: crate::shell::search::invocation_wake::waker(),
            };
            if self.send_index_job(job) {
                *self.surface_build.borrow_mut() = Some(receiver);
            }
        }
    }

    /// Hand one rebuild to the provider's index worker, starting it on the
    /// first rebuild (or again, once, if it has ended). One long-lived
    /// worker serves every surface generation rather than a thread each.
    fn send_index_job(&self, job: IndexJob) -> bool {
        let mut worker = self.index_worker.borrow_mut();
        let job = if let Some(jobs) = worker.as_ref() {
            match jobs.send(job) {
                Ok(()) => return true,
                Err(mpsc::SendError(job)) => job,
            }
        } else {
            job
        };
        *worker = Self::spawn_index_worker(
            self.broker.clone(),
            #[cfg(test)]
            Arc::clone(&self.build_probe),
        );
        worker.as_ref().is_some_and(|jobs| jobs.send(job).is_ok())
    }

    /// The worker behind [`Self::send_index_job`]. It ends when the provider
    /// drops its sender.
    fn spawn_index_worker(
        broker: AppActionBroker,
        #[cfg(test)] probe: Arc<BuildProbe>,
    ) -> Option<mpsc::Sender<IndexJob>> {
        let (jobs, queue) = mpsc::channel::<IndexJob>();
        std::thread::Builder::new()
            .name("tilo-live-action-index".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    let snapshot = Self::build_local_snapshot(
                        &broker,
                        &job.scope,
                        #[cfg(test)]
                        &probe,
                    );
                    if job.reply.send(snapshot).is_ok() {
                        job.wake.wake();
                    }
                }
            })
            .ok()?;
        Some(jobs)
    }

    fn local_action_candidates(
        &self,
        ctx: &SearchContext<'_>,
        max_results: usize,
    ) -> Vec<(PublishedSurface, PublishedAction, i32)> {
        let snapshot = self.surface_snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            return Vec::new();
        };
        if snapshot.scope != Self::surface_scope(ctx) {
            return Vec::new();
        }
        let Some(index) = (if ctx.include_programmatic {
            &snapshot.machine_index
        } else {
            &snapshot.index
        }) else {
            return snapshot
                .entries
                .iter()
                .take(max_results)
                .map(|(surface, action)| (surface.clone(), action.clone(), 0))
                .collect();
        };
        index
            .search(ctx.needle, Page::first(max_results))
            .hits
            .iter()
            .filter_map(|hit| {
                let at = usize::try_from(hit.key).ok()?;
                let (surface, action) = snapshot.entries.get(at)?;
                Some((surface.clone(), action.clone(), hit.rank.unwrap_or(0)))
            })
            .collect()
    }

    /// Reuse the established action-row constructor for one already-indexed
    /// action.  The temporary one-row projection keeps all typed callbacks,
    /// contracts, confirmation handling, and archived-launch behaviour in
    /// precisely one implementation while `query_page` accounts for this
    /// candidate explicitly.
    /// A disclosed directory descriptor is the canonical route for that
    /// exact publisher capability. Keep compatibility rows until it arrives.
    fn has_disclosed_directory_alias(
        &self,
        ctx: &SearchContext<'_>,
        surface: &PublishedSurface,
        action: &PublishedAction,
    ) -> bool {
        let root = self
            .broker
            .state()
            .action_directory_root_page(surface.peer_id.checked_sub(1), 1)
            .roots
            .into_iter()
            .find(|root| {
                root.peer_id == surface.peer_id
                    && root.app_id == surface.app_id
                    && root.cluster_id == surface.route.cluster_id
                    && root.record_id == surface.route.record_id
            });
        if root.is_none() {
            return false;
        }
        let actor = ctx
            .chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == surface.route.cluster_id)
            .map(|workspace| ctx.chrome.local_user_for_room(&workspace.room));
        let Some(actor) = actor else {
            return false;
        };
        let needle = ctx.needle.trim().to_lowercase();
        self.directory_pages.borrow().iter().any(|(key, page)| {
            key.0 == surface.peer_id
                && key.1 == actor
                && key.2 == needle
                && page.rows.iter().any(|row| {
                    row.app_id == surface.app_id
                        && row.action.as_ref().is_some_and(|descriptor| {
                            descriptor.address.resource == action.resource
                                && descriptor.address.action == action.action
                        })
                })
        })
    }

    fn materialize_local_action(
        &self,
        ctx: &SearchContext<'_>,
        surface: PublishedSurface,
        action: PublishedAction,
        rank: i32,
    ) -> Option<SearchResult> {
        let id = Self::result_id(&surface, &action);
        let scope = Self::surface_scope(ctx);
        let source_generation = self.broker.surfaces_generation();
        let temporary = SurfaceSnapshot {
            source_generation,
            scope,
            entries: vec![(surface, action)],
            index: None,
            machine_index: None,
        };
        let previous = self.surface_snapshot.replace(Some(temporary));
        let previous_projection = self.projection.replace(QueryProjection::LocalOnly);
        let results = self.query(*ctx);
        self.projection.set(previous_projection);
        self.surface_snapshot.replace(previous);
        let mut row = results.into_iter().find(|result| result.id == id)?;
        row.score = crate::shell::search::provider_score(rank);
        Some(row)
    }

    fn poll_directory_pages_at(&self, now: Instant) {
        let mut ready = Vec::new();
        let mut expired = false;
        self.directory_pending.borrow_mut().retain(|key, pending| {
            // The UI may have been idle after the reply arrived. The deadline
            // bounds unanswered work, not how soon a completed page is read.
            match crate::shell::search::invocation_wake::poll(
                &mut pending.receiver,
                &crate::shell::search::invocation_wake::waker(),
            ) {
                PendingPoll::Ready(Ok(page)) if !page.stale_cursor => {
                    ready.push((key.clone(), page, pending.initial_page));
                    false
                }
                PendingPoll::Ready(_) | PendingPoll::Abandoned => false,
                PendingPoll::Waiting if pending.deadline <= now => {
                    expired = true;
                    false
                }
                PendingPoll::Waiting => true,
            }
        });
        if expired {
            // An empty projection must not prevent another bounded request
            // after a stalled directory ticket has been cancelled.
            self.auxiliary_snapshot.borrow_mut().take();
            self.directory_epoch.fetch_add(1, Ordering::Relaxed);
        }
        if !ready.is_empty() {
            let mut pages = self.directory_pages.borrow_mut();
            for (key, page, initial_page) in ready {
                // A continuation only needs the page named by its opaque
                // cursor.  Superseded pages for this same route/query can be
                // dropped immediately; this bounds a 100k-row remote source
                // without making later rows unreachable.
                pages.retain(|existing, _| {
                    existing == &key
                        || existing.0 != key.0
                        || existing.1 != key.1
                        || existing.2 != key.2
                });
                if !pages.contains_key(&key)
                    && pages.len() >= MAX_RETAINED_DIRECTORY_PAGES
                    && let Some(evicted) = pages.keys().next().cloned()
                {
                    pages.remove(&evicted);
                }
                pages.insert(key, page);
                if initial_page {
                    self.directory_epoch.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A later page appends after a retained cursor. Drop only the
            // projection so that cursor can continue at its prior offset;
            // the source revision deliberately remains stable.
            self.auxiliary_snapshot.borrow_mut().take();
        }

        self.poll_service_pages_at(now);
    }

    fn poll_service_pages_at(&self, now: Instant) {
        let mut service_ready = Vec::new();
        let mut retry_projection = false;
        self.service_pending.borrow_mut().retain(|key, pending| {
            if pending.deadline <= now {
                self.report_service_diagnostic(key, 1 << 6, "service_page", "timed_out");
                retry_projection = true;
                return false;
            }
            match pending.receiver.poll_ready() {
                PendingPoll::Ready(Ok(page)) if !page.stale_cursor => {
                    service_ready.push((key.clone(), page));
                    false
                }
                PendingPoll::Ready(Ok(_)) => {
                    self.report_service_diagnostic(key, 1 << 5, "service_page", "stale");
                    retry_projection = true;
                    false
                }
                PendingPoll::Ready(Err(_)) => {
                    self.report_service_diagnostic(key, 1 << 4, "service_page", "failed");
                    retry_projection = true;
                    false
                }
                PendingPoll::Abandoned => {
                    self.report_service_diagnostic(key, 1 << 7, "service_page", "uncertain");
                    retry_projection = true;
                    false
                }
                PendingPoll::Waiting => true,
            }
        });
        if retry_projection {
            // An unanswered service page is not an authoritative empty result.
            // Retire the projection so the next query owes a fresh bounded page.
            self.auxiliary_snapshot.borrow_mut().take();
            self.directory_epoch.fetch_add(1, Ordering::Relaxed);
        }
        if !service_ready.is_empty() {
            let mut pages = self.service_pages.borrow_mut();
            let mut visible_projection_changed = false;
            for (key, page) in service_ready {
                // Closed diagnostics distinguish a genuinely empty service
                // projection from a first page stopped before service rows.
                // Never log query text, identities, rows or opaque revisions.
                tracing::debug!(
                    target: SERVICE_DIAGNOSTIC_TARGET,
                    stage = "service_page_shape",
                    empty_needle = key.2.is_empty(),
                    row_count = page.rows.len(),
                    workspace_row_count = page.rows.iter().filter(|row| {
                        tilo_actions::remote::workspace_provider_room(&row.provider)
                            == Some(key.0.as_str())
                    }).count(),
                    exhausted = page.exhausted,
                    next_cursor_present = !page.next_cursor.is_empty(),
                    budget_exhausted = page.budget_exhausted,
                    revision_changed = pages.get(&key)
                        .is_some_and(|previous| previous.revision != page.revision),
                    "native workspace service catalogue shape"
                );
                let page_is_visible = |page: &CataloguePage| {
                    page.rows.iter().any(|row| {
                        tilo_actions::remote::workspace_provider_room(&row.provider)
                            == Some(key.0.as_str())
                    })
                };
                visible_projection_changed |=
                    pages.get(&key).is_some_and(page_is_visible) || page_is_visible(&page);
                self.report_service_diagnostic(
                    &key,
                    if page_is_visible(&page) {
                        1 << 2
                    } else {
                        1 << 3
                    },
                    "service_page",
                    if page_is_visible(&page) {
                        "ready_visible"
                    } else {
                        "ready_empty"
                    },
                );
                if !pages.contains_key(&key)
                    && pages.len() >= MAX_RETAINED_SERVICE_PAGES
                    && let Some(evicted) = pages.keys().next().cloned()
                {
                    pages.remove(&evicted);
                }
                pages.insert(key, page);
            }
            if visible_projection_changed {
                self.directory_epoch.fetch_add(1, Ordering::Relaxed);
                self.auxiliary_snapshot.borrow_mut().take();
            }
        }
    }

    fn poll_directory_pages(&self) {
        self.poll_directory_pages_at(Instant::now());
    }

    fn set_directory_query(&self, needle: &str) {
        let next = needle.to_string();
        let mut current = self.directory_query.borrow_mut();
        if current.as_ref() == Some(&next) {
            return;
        }
        *current = Some(next.clone());
        // Dropping stale receivers explicitly cancels their shell-side
        // tickets. The LocalSet may still finish its bounded work, but cannot
        // grow this UI cache or revive an old query's results.
        self.directory_pages
            .borrow_mut()
            .retain(|(_, _, cached_needle, _), _| cached_needle == &next);
        self.directory_pending
            .borrow_mut()
            .retain(|(_, _, cached_needle, _), _| cached_needle == &next);
        self.service_pages
            .borrow_mut()
            .retain(|(_, _, cached_needle), _| cached_needle == &next);
        self.service_pending
            .borrow_mut()
            .retain(|(_, _, cached_needle), _| cached_needle == &next);
        self.service_diagnostics
            .borrow_mut()
            .retain(|(_, _, cached_needle), _| cached_needle == &next);
        self.directory_epoch.fetch_add(1, Ordering::Relaxed);
    }

    fn set_directory_context(&self, ctx: &SearchContext<'_>, needle: &str) {
        let scoped_query = if let Some(scope) = &ctx.chrome.search_resource_scope {
            format!(
                "{needle}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
                scope.peer_id, scope.resource
            )
        } else {
            needle.to_string()
        };
        self.set_directory_query(&scoped_query);
        let current = {
            let tree = ctx.chrome.tree();
            tree.desktop.active_workspace().map(|workspace| {
                (
                    workspace.room.to_string(),
                    ctx.chrome.local_user_for_room(&workspace.room),
                    tree.desktop.external_revision,
                    tree.desktop.room_epoch,
                )
            })
        };
        // This is the providers-only projection's stamp, not the eager rows'
        // digest. Sample after releasing the tree guard: admission re-reads it.
        let source_revision = current.as_ref().and_then(|(room, actor, _, _)| {
            let session = self.action_sessions.open(room, Some(actor)).ok()?;
            let mut answer = session.query(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            });
            match answer.poll_ready() {
                PendingPoll::Ready(Ok(page)) if !page.stale_cursor => Some(page.revision),
                _ => None,
            }
        });
        let previous_service_context = self.service_context.borrow().clone();
        if previous_service_context.as_ref() != current.as_ref()
            || *self.service_source_revision.borrow() != source_revision
        {
            self.service_pages.borrow_mut().clear();
            self.service_pending.borrow_mut().clear();
            self.service_diagnostics.borrow_mut().clear();
            if previous_service_context.is_some() {
                self.directory_epoch.fetch_add(1, Ordering::Relaxed);
            }
            self.service_context.borrow_mut().clone_from(&current);
            self.service_source_revision.replace(source_revision);
        }
        self.service_pages
            .borrow_mut()
            .retain(|(room, actor, _), _| {
                current
                    .as_ref()
                    .is_some_and(|(current_room, current_actor, _, _)| {
                        room == current_room && actor == current_actor
                    })
            });
        self.service_pending
            .borrow_mut()
            .retain(|(room, actor, _), _| {
                current
                    .as_ref()
                    .is_some_and(|(current_room, current_actor, _, _)| {
                        room == current_room && actor == current_actor
                    })
            });
    }

    fn apply_directory_invalidations(&self) {
        let invalidated = std::mem::take(&mut *self.directory_invalidations.borrow_mut());
        if invalidated.is_empty() {
            return;
        }
        let mut pages = self.directory_pages.borrow_mut();
        let mut pending = self.directory_pending.borrow_mut();
        for key in invalidated {
            pages.remove(&key);
            pending.remove(&key);
        }
        self.directory_epoch.fetch_add(1, Ordering::Relaxed);
    }

    fn apply_service_invalidations(&self) {
        let invalidated = std::mem::take(&mut *self.service_invalidations.borrow_mut());
        if invalidated.is_empty() {
            return;
        }
        let mut pages = self.service_pages.borrow_mut();
        let mut pending = self.service_pending.borrow_mut();
        for key in invalidated {
            pages.remove(&key);
            pending.remove(&key);
        }
        self.directory_epoch.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one bounded remote-directory continuation is assembled atomically so cache, scope, actor, and cursor semantics cannot diverge"
    )]
    fn prepare_auxiliary_snapshot(
        &self,
        ctx: &SearchContext<'_>,
        needle: &str,
        revision: &str,
        root_limit: usize,
        resume: &AuxiliaryResume,
    ) {
        let scope = Self::surface_scope(ctx);
        if self
            .auxiliary_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.revision == revision
                    && snapshot.query == needle
                    && snapshot.scope == scope
                    && snapshot.resume == *resume
            })
        {
            return;
        }

        // A palette turn fans out to a bounded root page only.  Directory
        // replies arrive asynchronously and bump `directory_epoch`; that
        // makes the next turn rebuild this small retained projection instead
        // of scanning a remote corpus while the user types.
        let (root_page, root_next_after) = self.directory_root_page(
            ctx,
            resume.root_after,
            root_limit.min(MAX_DIRECTORY_ROOTS_PER_QUERY),
        );
        let mut roots = resume
            .directory
            .as_ref()
            .map_or(root_page, |directory| vec![directory.root.clone()]);
        if let Some(resource_scope) = &ctx.chrome.search_resource_scope {
            roots.retain(|root| {
                root.peer_id == resource_scope.peer_id
                    && root.cluster_id == resource_scope.cluster_id
                    && root.app_id == resource_scope.app_id
            });
        }
        let parent_resource = ctx
            .chrome
            .search_resource_scope
            .as_ref()
            .map_or("", |scope| scope.resource.as_str());
        let mut entries = Vec::new();
        let mut pending_directory = Vec::new();
        let mut next_resume = None;
        let actor = ctx.chrome.active_local_user();
        if ctx.chrome.search_resource_scope.is_none()
            && (needle.is_empty() || "jobs status progress".contains(needle))
        {
            for (id, update) in self
                .broker
                .statuses(&actor)
                .into_iter()
                .take(MAX_JOB_ROWS_PER_QUERY)
            {
                if entries.len() >= root_limit {
                    break;
                }
                let state = job_state(&update);
                entries.push(AuxiliaryEntry::Job {
                    id,
                    state,
                    cancel: false,
                    actor: actor.clone(),
                });
                if self.broker.can_cancel(id, &actor) && entries.len() < root_limit {
                    entries.push(AuxiliaryEntry::Job {
                        id,
                        state,
                        cancel: true,
                        actor: actor.clone(),
                    });
                }
            }
        }
        // A queued native writer must be able to run before the provider
        // callbacks below re-read membership through this same tree.
        let workspace_room = if resume.root_after.is_none()
            && resume.directory.is_none()
            && ctx.chrome.search_resource_scope.is_none()
        {
            let tree = ctx.chrome.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.room)
        } else {
            None
        };
        if let Some(workspace_room) = workspace_room {
            let room = workspace_room.to_string();
            let workspace_actor = ctx.chrome.local_user_for_room(&workspace_room);
            let key = (room.clone(), workspace_actor.clone(), needle.to_string());
            #[cfg(test)]
            if let Some(probe) = self.auxiliary_service_probe.borrow_mut().take() {
                probe();
            }
            self.request_service_page(&room, &workspace_actor, needle);
            if let Some(page) = self.service_pages.borrow().get(&key) {
                for row in &page.rows {
                    if entries.len() >= root_limit {
                        break;
                    }
                    if tilo_actions::remote::workspace_provider_room(&row.provider)
                        != Some(room.as_str())
                    {
                        continue;
                    }
                    entries.push(AuxiliaryEntry::Service {
                        room: room.clone(),
                        revision: page.revision.clone(),
                        row: row.clone(),
                        actor: workspace_actor.clone(),
                    });
                }
            }
        }
        for root in roots {
            if entries.len() >= root_limit {
                next_resume = Some(AuxiliaryResume {
                    root_after: resume.root_after,
                    directory: Some(DirectoryContinuation {
                        root,
                        actor: String::new(),
                        cursor: Vec::new(),
                        row_offset: 0,
                    }),
                });
                break;
            }
            if !matches!(
                ctx.effective_scope(),
                crate::shell::chrome_state::SearchScope::All
            ) && ctx
                .chrome
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
                != Some(root.cluster_id.clone())
            {
                continue;
            }
            let root_actor = ctx
                .chrome
                .tree()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == root.cluster_id)
                .map(|workspace| ctx.chrome.local_user_for_room(&workspace.room));
            let Some(root_actor) = root_actor else {
                continue;
            };
            let mut remote_cursor = resume
                .directory
                .as_ref()
                .filter(|resume| resume.root.peer_id == root.peer_id)
                .map(|resume| resume.cursor.clone())
                .unwrap_or_default();
            let mut row_offset = resume
                .directory
                .as_ref()
                .filter(|resume| resume.root.peer_id == root.peer_id)
                .map_or(0, |resume| resume.row_offset);
            loop {
                self.request_directory_page(
                    root.peer_id,
                    &root.cluster_id,
                    &root_actor,
                    needle,
                    &remote_cursor,
                    parent_resource,
                );
                let page = self
                    .directory_pages
                    .borrow()
                    .get(&(
                        root.peer_id,
                        root_actor.clone(),
                        needle.to_string(),
                        remote_cursor.clone(),
                    ))
                    .cloned();
                let Some(page) = page else { break };
                let rows = page.rows;
                let mut consumed = row_offset.min(rows.len());
                for row in rows.into_iter().skip(row_offset) {
                    if entries.len() >= root_limit {
                        next_resume = Some(AuxiliaryResume {
                            root_after: resume.root_after,
                            directory: Some(DirectoryContinuation {
                                root: root.clone(),
                                actor: root_actor.clone(),
                                cursor: remote_cursor.clone(),
                                row_offset: consumed,
                            }),
                        });
                        break;
                    }
                    consumed = consumed.saturating_add(1);
                    if let Some(descriptor) = row.action.clone() {
                        entries.push(AuxiliaryEntry::Directory {
                            root: root.clone(),
                            revision: page.revision.clone(),
                            descriptor,
                            actor: root_actor.clone(),
                            cursor: remote_cursor.clone(),
                        });
                    } else if row.has_children {
                        entries.push(AuxiliaryEntry::Resource {
                            root: root.clone(),
                            row,
                        });
                    }
                }
                if next_resume.is_some() {
                    break;
                }
                if page.exhausted || page.next_cursor.is_empty() {
                    break;
                }
                remote_cursor = page.next_cursor;
                row_offset = 0;
                let next_key = (
                    root.peer_id,
                    root_actor.clone(),
                    needle.to_string(),
                    remote_cursor.clone(),
                );
                if !self.directory_pages.borrow().contains_key(&next_key) {
                    pending_directory.push(DirectoryContinuation {
                        root: root.clone(),
                        actor: root_actor.clone(),
                        cursor: remote_cursor,
                        row_offset: 0,
                    });
                    break;
                }
            }
            if next_resume.is_some() {
                break;
            }
        }
        if next_resume.is_none()
            && pending_directory.is_empty()
            && resume
                .directory
                .as_ref()
                .map_or(root_next_after, |directory| Some(directory.root.peer_id))
                .is_some()
        {
            next_resume = Some(AuxiliaryResume {
                root_after: resume
                    .directory
                    .as_ref()
                    .map(|directory| directory.root.peer_id)
                    .or(root_next_after),
                directory: None,
            });
        }
        *self.auxiliary_snapshot.borrow_mut() = Some(AuxiliarySnapshot {
            revision: revision.to_string(),
            query: needle.to_string(),
            scope,
            resume: resume.clone(),
            entries,
            pending_directory,
            next_resume,
        });
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the broker handle is cloned once per callback; each clone moves into its own closure"
    )]
    fn job_result(&self, id: u64, state: &'static str, cancel: bool, actor: &str) -> SearchResult {
        if cancel {
            let broker = self.broker.clone();
            let actor = actor.to_string();
            let invoke_broker = broker.clone();
            let invoke_actor = actor.clone();
            return SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: PROVIDER_ID.into(),
                id: job_cancel_id(id),
                title: t_args("search-app-job-cancel", &[("id", id.to_string().into())]),
                subtitle: Some(t("search-app-job-cancel-detail")),
                badge: None,
                score: 5,
                params: Vec::new(),
                contract: None,
                on_open: std::rc::Rc::new(move |_, _, chrome, _| {
                    if chrome.borrow().active_local_user() != actor {
                        return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                    }
                    broker.cancel(id, &actor).map_err(ActionError::Refused)
                }),
                on_invoke: Some(std::rc::Rc::new(move |_, _, chrome, _| {
                    if chrome.borrow().active_local_user() != invoke_actor {
                        return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                    }
                    invoke_broker
                        .cancel(id, &invoke_actor)
                        .map_err(ActionError::Refused)?;
                    let update = invoke_broker
                        .status_for_actor(id, &invoke_actor)
                        .ok_or_else(|| ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()))?;
                    Ok(ActionReceipt {
                        broker_invocation_id: Some(id),
                        update,
                    })
                })),
                on_begin_invoke: None,
            };
        }
        let status_broker = self.broker.clone();
        let actor = actor.to_string();
        SearchResult {
            kind: ResourceKind::Custom("action-job".into()),
            operation_kind: tilo_actions::OperationKind::View,
            provider_id: PROVIDER_ID.into(),
            id: job_status_id(id),
            title: t_args("search-app-job", &[("id", id.to_string().into())]),
            subtitle: Some(job_state_label(state)),
            badge: None,
            score: -10,
            params: Vec::new(),
            contract: None,
            on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
            on_invoke: Some(std::rc::Rc::new(move |_, _, chrome, _| {
                if chrome.borrow().active_local_user() != actor {
                    return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                }
                let update = status_broker
                    .status_for_actor(id, &actor)
                    .ok_or_else(|| ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()))?;
                Ok(ActionReceipt {
                    broker_invocation_id: Some(id),
                    update,
                })
            })),
            on_begin_invoke: None,
        }
    }

    fn certified_job_result(
        &self,
        id: u64,
        state: &'static str,
        cancel: bool,
        room: &str,
        actor: &str,
    ) -> SearchResult {
        let source = self.action_sessions.clone();
        let room = room.to_string();
        let actor = actor.to_string();
        let invoke_source = self.action_sessions.clone();
        let invoke_room = room.clone();
        let invoke_actor = actor.clone();
        let operation_kind = if cancel {
            tilo_actions::OperationKind::Effect
        } else {
            tilo_actions::OperationKind::View
        };
        SearchResult {
            kind: if cancel {
                ResourceKind::Action
            } else {
                ResourceKind::Custom("action-job".into())
            },
            operation_kind,
            provider_id: PROVIDER_ID.into(),
            id: if cancel {
                job_cancel_id(id)
            } else {
                job_status_id(id)
            },
            title: if cancel {
                t_args("search-app-job-cancel", &[("id", id.to_string().into())])
            } else {
                t_args("search-app-job", &[("id", id.to_string().into())])
            },
            subtitle: Some(if cancel {
                t("search-app-job-cancel-detail")
            } else {
                job_state_label(state)
            }),
            badge: None,
            score: if cancel { 5 } else { -10 },
            params: Vec::new(),
            contract: None,
            on_open: std::rc::Rc::new(move |_, _, chrome, _| {
                if !cancel {
                    return Ok(());
                }
                let current = chrome.borrow();
                let current_room = current
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room.to_string());
                if current.active_local_user() != actor || current_room.as_deref() != Some(&room) {
                    return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                }
                drop(current);
                let session = source
                    .open(&room, Some(&actor))
                    .map_err(|_| ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()))?;
                session
                    .cancel_job(id)
                    .map_err(|_| ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()))
            }),
            on_invoke: Some(std::rc::Rc::new(move |_, _, chrome, _| {
                let current = chrome.borrow();
                let current_room = current
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room.to_string());
                if current.active_local_user() != invoke_actor {
                    report_certified_job_refusal("active_actor_changed");
                    return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                }
                if current_room.as_deref() != Some(&invoke_room) {
                    report_certified_job_refusal("active_room_changed");
                    return Err(ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()));
                }
                drop(current);
                let session = invoke_source
                    .open_classified(&invoke_room, Some(&invoke_actor))
                    .map_err(|(_, outcome)| {
                        report_certified_job_refusal(outcome);
                        ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into())
                    })?;
                if cancel {
                    session
                        .cancel_job(id)
                        .map_err(|_| ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into()))?;
                }
                let update = session.job_status_classified(id).map_err(|outcome| {
                    report_certified_job_refusal(outcome);
                    ActionError::Refused(JOB_UNAVAILABLE_MESSAGE.into())
                })?;
                drop(session);
                Ok(ActionReceipt {
                    broker_invocation_id: Some(id),
                    update,
                })
            })),
            on_begin_invoke: None,
        }
    }

    /// Exact receipt reads use the actor-owned broker key, not the four-row
    /// palette preview or the currently browsed application resource parent.
    fn job_control_page(
        &self,
        ctx: &SearchContext<'_>,
        target: (u64, bool),
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let revision = self.search_revision(ctx);
        if cursor.is_some() {
            return SearchPage::stale(revision);
        }
        let (id, cancel) = target;
        let actor = ctx.chrome.active_local_user();
        let room = ctx
            .chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        let mut page = SearchPage::exhausted(revision, Vec::new());
        if !ctx.wanted.is_empty() && !ctx.wanted.contains(&ResourceKind::Action) {
            return page;
        }
        let certified = room.as_deref().and_then(|room| {
            let session = self.action_sessions.open(room, Some(&actor)).ok()?;
            let update = session.job_status(id)?;
            (!cancel || session.can_cancel_job(id)).then_some((room, update))
        });
        let row = if let Some((room, update)) = certified {
            self.certified_job_result(id, job_state(&update), cancel, room, &actor)
        } else {
            // A remote invocation may execute under an internal Local owner
            // (workspace services do). Never let that implementation detail
            // downgrade its exact certificate/device binding to actor text.
            if self.broker.job_is_remotely_admitted(id) != Some(false) {
                return page;
            }
            let Some(update) = self.broker.status_for_actor(id, &actor) else {
                return page;
            };
            if cancel && !self.broker.can_cancel(id, &actor) {
                return page;
            }
            self.job_result(id, job_state(&update), cancel, &actor)
        };
        let bytes = row
            .title
            .len()
            .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
        if bytes > budget.max_bytes {
            page.budget_exhausted = true;
        } else {
            page.results.push(row);
        }
        page
    }

    fn live_page(
        revision: String,
        results: Vec<SearchResult>,
        stage: &str,
        after: usize,
        needle: &str,
        scope: &str,
    ) -> SearchPage {
        let after = if stage == "local" && after == usize::MAX {
            "-".to_string()
        } else {
            after.to_string()
        };
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: Some(SearchCursor {
                revision,
                token: format!(
                    "live:{stage}{ID_SEPARATOR}{after}{ID_SEPARATOR}{needle}{ID_SEPARATOR}{scope}"
                ),
            }),
            exhausted: false,
            stale_cursor: false,
            budget_exhausted: true,
        }
    }

    fn live_aux_page(
        revision: String,
        results: Vec<SearchResult>,
        after: usize,
        needle: &str,
        scope: &str,
        resume: &AuxiliaryResume,
    ) -> SearchPage {
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: Some(SearchCursor {
                revision,
                token: format!(
                    "live:aux{ID_SEPARATOR}{after}{ID_SEPARATOR}{needle}{ID_SEPARATOR}{}{ID_SEPARATOR}{scope}",
                    Self::encode_auxiliary_resume(resume),
                ),
            }),
            exhausted: false,
            stale_cursor: false,
            budget_exhausted: true,
        }
    }

    /// Keep a continuation self-contained.  The old cursor table was capped
    /// at 128 entries and silently made a long, otherwise valid directory
    /// unreachable.  This contains only inert route metadata and the remote
    /// directory's opaque cursor; the revision check still rejects it when
    /// either root ordering or disclosure changes.
    fn encode_auxiliary_resume(resume: &AuxiliaryResume) -> String {
        use base64::Engine as _;
        let root_after = resume
            .root_after
            .map_or_else(String::new, |id| id.to_string());
        let directory = resume
            .directory
            .as_ref()
            .map_or_else(String::new, |directory| {
                let cursor =
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&directory.cursor);
                [
                    directory.root.peer_id.to_string(),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(directory.root.app_id.as_bytes()),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(directory.root.cluster_id.as_bytes()),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(directory.root.record_id.as_bytes()),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(directory.actor.as_bytes()),
                    cursor,
                    directory.row_offset.to_string(),
                ]
                .join(",")
            });
        format!("{root_after};{directory}")
    }

    fn decode_auxiliary_resume(encoded: &str) -> Option<AuxiliaryResume> {
        use base64::Engine as _;
        let (root_after, directory) = encoded.split_once(';')?;
        let root_after = (!root_after.is_empty())
            .then(|| root_after.parse::<u64>().ok())
            .flatten();
        if directory.is_empty() {
            return Some(AuxiliaryResume {
                root_after,
                directory: None,
            });
        }
        let mut fields = directory.split(',');
        let peer_id = fields.next()?.parse().ok()?;
        let decode = |field: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(field)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        };
        let app_id = decode(fields.next()?)?;
        let cluster_id = decode(fields.next()?)?;
        let record_id = decode(fields.next()?)?;
        let actor = decode(fields.next()?)?;
        let cursor = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(fields.next()?)
            .ok()?;
        let row_offset = fields.next()?.parse().ok()?;
        fields.next().is_none().then_some(AuxiliaryResume {
            root_after,
            directory: Some(DirectoryContinuation {
                root: crate::shell::collab_broker_capnp::ActionDirectoryRoot {
                    peer_id,
                    app_id,
                    cluster_id,
                    record_id,
                },
                actor,
                cursor,
                row_offset,
            }),
        })
    }

    fn request_directory_page(
        &self,
        peer_id: u64,
        cluster_id: &str,
        actor: &str,
        needle: &str,
        cursor: &[u8],
        parent_resource: &str,
    ) {
        let key = (
            peer_id,
            actor.to_string(),
            needle.to_string(),
            cursor.to_vec(),
        );
        if self.directory_pages.borrow().contains_key(&key)
            || self.directory_pending.borrow().contains_key(&key)
        {
            return;
        }
        let query = CatalogueQuery {
            provider: Some(peer_id.to_string()),
            parent_resource: (!parent_resource.is_empty()).then(|| parent_resource.to_string()),
            needle: needle.to_string(),
            cursor: cursor.to_vec(),
            max_results: 48,
            limits: Some(CatalogueLimits {
                max_bytes: Some(48 * 1024),
                max_work: Some(4_096),
                max_depth: Some(1),
            }),
            ..CatalogueQuery::default()
        };
        #[cfg(test)]
        self.directory_query_probe.replace(Some(query.clone()));
        let Ok(session) = self.action_sessions.open(cluster_id, Some(actor)) else {
            return;
        };
        self.directory_pending.borrow_mut().insert(
            key,
            PendingDirectoryPage {
                // Its answer wakes the loop: a listing waiting to be complete
                // is answered when the page is in.
                receiver: crate::shell::search::invocation_wake::watch(session.query(query)),
                deadline: deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
                initial_page: cursor.is_empty(),
            },
        );
    }

    /// Request the eager broker rows for one exact workspace. Native
    /// services publish opaque providers here, so they cannot be discovered
    /// through the numeric application-directory root list.
    fn request_service_page(&self, room: &str, actor: &str, needle: &str) {
        let key = (room.to_string(), actor.to_string(), needle.to_string());
        if self.service_pages.borrow().contains_key(&key)
            || self.service_pending.borrow().contains_key(&key)
        {
            return;
        }
        let query = CatalogueQuery {
            needle: needle.to_string(),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            limits: Some(CatalogueLimits {
                max_bytes: Some(256 * 1024),
                max_work: Some(8_192),
                max_depth: Some(1),
            }),
            ..CatalogueQuery::default()
        };
        let session = match self.action_sessions.open_classified(room, Some(actor)) {
            Ok(session) => {
                self.report_service_diagnostic(&key, 1, "service_session", "opened");
                session
            }
            Err((_, outcome)) => {
                self.report_service_diagnostic(&key, 1 << 1, "service_session", outcome);
                return;
            }
        };
        if self.service_pending.borrow().len() >= MAX_RETAINED_SERVICE_PAGES
            && let Some(evicted) = self.service_pending.borrow().keys().next().cloned()
        {
            self.service_pending.borrow_mut().remove(&evicted);
        }
        self.service_pending.borrow_mut().insert(
            key,
            PendingDirectoryPage {
                receiver: session.query(query),
                deadline: deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
                initial_page: true,
            },
        );
    }

    fn report_service_diagnostic(
        &self,
        key: &ServiceCacheKey,
        bit: u16,
        stage: &'static str,
        outcome: &'static str,
    ) {
        let mut diagnostics = self.service_diagnostics.borrow_mut();
        if !diagnostics.contains_key(key)
            && diagnostics.len() >= MAX_RETAINED_SERVICE_PAGES
            && let Some(evicted) = diagnostics.keys().next().cloned()
        {
            diagnostics.remove(&evicted);
        }
        let observed = diagnostics.entry(key.clone()).or_default();
        if *observed & bit != 0 {
            return;
        }
        *observed |= bit;
        tracing::debug!(
            target: SERVICE_DIAGNOSTIC_TARGET,
            stage,
            outcome,
            "native workspace service catalogue observation"
        );
    }

    /// The browsable application directories in each workspace, as the
    /// action host offers them to the local user of that workspace,
    /// ordered by publisher.
    fn directory_roots(&self, ctx: &SearchContext<'_>) -> (Vec<ActionDirectoryRoot>, String) {
        self.directory_roots_in(ctx.chrome)
    }

    /// [`Self::directory_roots`] as `chrome` sees them.
    fn directory_roots_in(
        &self,
        chrome: &crate::shell::chrome_state::ChromeState,
    ) -> (Vec<ActionDirectoryRoot>, String) {
        let mut roots = Vec::new();
        let mut revision = String::new();
        let workspace_rooms = chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.room)
            .collect::<Vec<_>>();
        for workspace_room in workspace_rooms {
            let room = workspace_room.to_string();
            let expected = chrome.local_user_for_room(&workspace_room);
            let Ok(session) = self.action_sessions.open(&room, Some(&expected)) else {
                continue;
            };
            let mut providers = session.query(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            });
            // Listing publishers is answered from memory, never an app.
            let PendingPoll::Ready(Ok(page)) = providers.poll_ready() else {
                continue;
            };
            revision = page.revision;
            roots.extend(
                page.providers
                    .into_iter()
                    .filter(|provider| provider.browsable && provider.room == room)
                    .filter_map(|provider| {
                        Some(ActionDirectoryRoot {
                            peer_id: provider.provider.parse().ok()?,
                            app_id: provider.app_id,
                            cluster_id: provider.room,
                            record_id: provider.record_id,
                        })
                    }),
            );
        }
        roots.sort_by_key(|root| root.peer_id);
        roots.dedup_by_key(|root| root.peer_id);
        (roots, revision)
    }

    /// What [`Self::directory_roots_in`] answers changes with, read without
    /// opening a certified session: the broker's published surfaces and
    /// directory roots, and per workspace its local Person and that Person's
    /// identity files. Stamps and counters only, so the palette can ask on
    /// every tick.
    fn directory_revision(&self, chrome: &crate::shell::chrome_state::ChromeState) -> String {
        use std::hash::{Hash as _, Hasher as _};
        /// Moves on every call while some identity can't be trusted to its
        /// stamp, so such a revision never matches the previous one.
        static UNSETTLED: AtomicU64 = AtomicU64::new(0);
        let rooms = chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.room)
            .collect::<Vec<_>>();
        let mut identities = std::collections::hash_map::DefaultHasher::new();
        for room in rooms {
            let name = room.to_string();
            let Some(identity) = self.action_sessions.identity_revision(&name) else {
                return format!("unsettled-{}", UNSETTLED.fetch_add(1, Ordering::Relaxed));
            };
            (name, chrome.local_user_for_room(&room), identity).hash(&mut identities);
        }
        format!(
            "{}.{}.{:016x}",
            self.broker.surfaces_generation(),
            self.broker.action_directory_root_page(None, 0).generation,
            identities.finish()
        )
    }

    /// The drill-down scope for `resource` of the app `app_id` in the active
    /// workspace, labelled `label`, as entering it from a directory row
    /// would set it. `None` while the app publishes no browsable directory
    /// there.
    pub(crate) fn resource_scope_for(
        &self,
        chrome: &crate::shell::chrome_state::ChromeState,
        app_id: &str,
        resource: &str,
        label: &str,
    ) -> Option<crate::shell::chrome_state::SearchResourceScope> {
        let workspace_room = chrome.tree().desktop.active_workspace()?.room.to_string();
        let root = self
            .directory_roots_in(chrome)
            .0
            .into_iter()
            .find(|root| root.app_id == app_id && root.cluster_id == workspace_room)?;
        Some(crate::shell::chrome_state::SearchResourceScope {
            peer_id: root.peer_id,
            cluster_id: root.cluster_id,
            app_id: root.app_id,
            resource: resource.to_owned(),
            label: label.to_owned(),
            kind: String::new(),
        })
    }

    /// One bounded page of [`Self::directory_roots`] after `after_peer`,
    /// with the continuation for the next.
    fn directory_root_page(
        &self,
        ctx: &SearchContext<'_>,
        after_peer: Option<u64>,
        limit: usize,
    ) -> (Vec<ActionDirectoryRoot>, Option<u64>) {
        let mut remaining = self
            .directory_roots(ctx)
            .0
            .into_iter()
            .filter(|root| after_peer.is_none_or(|after| root.peer_id > after));
        let page = remaining.by_ref().take(limit).collect::<Vec<_>>();
        let next = remaining
            .next()
            .and_then(|_| page.last().map(|root| root.peer_id));
        (page, next)
    }

    fn result_parts(id: &str) -> Option<(&str, &str, &str, &str, &str, &str)> {
        let mut parts = id.split(ID_SEPARATOR);
        let app = parts.next()?;
        let permission = parts.next()?;
        let cluster = parts.next()?;
        let record = parts.next()?;
        let resource = parts.next()?;
        let action = parts.next()?;
        parts
            .next()
            .is_none()
            .then_some((app, permission, cluster, record, resource, action))
    }

    pub fn capability_target(
        id: &str,
    ) -> Option<(&str, tilo_actions::PermissionRequirement, &str, &str)> {
        let mut service = id.split(ID_SEPARATOR);
        if service.next() == Some("service") {
            let _provider = service.next()?;
            let revision = service.next()?;
            let app = service.next()?;
            let permission = Self::permission_from_id(service.next()?)?;
            let resource = service.next()?;
            let action = service.next()?;
            if service.next().is_some()
                || revision.is_empty()
                || app.is_empty()
                || resource.is_empty()
                || action.is_empty()
            {
                return None;
            }
            return Some((app, permission, resource, action));
        }
        let (app, permission, _cluster, _record, resource, action) = Self::result_parts(id)?;
        Some((app, Self::permission_from_id(permission)?, resource, action))
    }

    fn permission_from_id(permission: &str) -> Option<tilo_actions::PermissionRequirement> {
        Some(match permission {
            "none" => tilo_actions::PermissionRequirement::None,
            "read" => tilo_actions::PermissionRequirement::Read,
            "comment" => tilo_actions::PermissionRequirement::Comment,
            "write" => tilo_actions::PermissionRequirement::Write,
            "invite" => tilo_actions::PermissionRequirement::Invite,
            "admin" => tilo_actions::PermissionRequirement::Admin,
            _ => return None,
        })
    }

    fn permission_id(permission: tilo_actions::PermissionRequirement) -> &'static str {
        match permission {
            tilo_actions::PermissionRequirement::None => "none",
            tilo_actions::PermissionRequirement::Read => "read",
            tilo_actions::PermissionRequirement::Comment => "comment",
            tilo_actions::PermissionRequirement::Write => "write",
            tilo_actions::PermissionRequirement::Invite => "invite",
            tilo_actions::PermissionRequirement::Admin => "admin",
        }
    }

    /// The application, room, record, resource, and operation represented by
    /// one provider-local id.
    ///
    /// The palette needs only the opaque id. Headless integrations also need
    /// its existing resource hierarchy so they can reveal one application,
    /// then one object, without flattening every action into their initial
    /// context. Keep the decoding beside `Self::result_id`, where the wire
    /// address is minted, rather than making those integrations duplicate it.
    pub fn hierarchy(id: &str) -> Option<(&str, &str, &str, &str, &str)> {
        let (app, _permission, cluster, record, resource, action) = Self::result_parts(id)?;
        Some((app, cluster, record, resource, action))
    }

    fn result_id(surface: &PublishedSurface, action: &PublishedAction) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}",
            surface.app_id,
            ID_SEPARATOR,
            Self::permission_id(action.contract.permission),
            ID_SEPARATOR,
            surface.route.cluster_id,
            ID_SEPARATOR,
            surface.route.record_id,
            ID_SEPARATOR,
            action.resource,
            ID_SEPARATOR,
            action.action
        )
    }

    fn action_params(action: &PublishedAction) -> Vec<ActionParam> {
        let mut params = if action.params.is_empty() {
            match action.input {
                PublishedInput::None => Vec::new(),
                PublishedInput::Text {
                    wants_content: true,
                    ..
                } => vec![ActionParam::required(
                    "content",
                    ParamType::Text,
                    "Content supplied to the application action",
                )],
                PublishedInput::Text { .. } => vec![ActionParam::required(
                    "argument",
                    ParamType::Text,
                    "Text supplied to the application action",
                )],
            }
        } else {
            action.params.clone()
        };
        push_confirmation_params(&mut params, &action.contract.advertised_confirmation());
        params
    }

    fn directory_action_result(
        &self,
        root: &ActionDirectoryRoot,
        revision: &str,
        descriptor: &tilo_actions::ActionDescriptor,
        needle: &str,
        actor: &str,
        cursor: &[u8],
    ) -> SearchResult {
        // Exact addresses also materialize programmatic actions; browse
        // callers filter their visibility before reaching this shared path.
        // The app's own index found this row for the needle.
        let score = crate::shell::search::remote_score(
            needle,
            &descriptor.action_label,
            &descriptor.resource_label,
        );
        let contract = descriptor.contract.clone();
        let mut params = descriptor.params.clone();
        push_confirmation_params(&mut params, &contract.advertised_confirmation());
        let action_sessions = self.action_sessions.clone();
        let peer_id = root.peer_id;
        let cluster_id = root.cluster_id.clone();
        let revision = revision.to_string();
        let address = descriptor.address.clone();
        let actor = actor.to_string();
        let invalidations = Rc::clone(&self.directory_invalidations);
        let cache_key = (peer_id, actor.clone(), needle.to_string(), cursor.to_vec());
        let typed = !descriptor.params.is_empty();
        let resource = address.resource.clone();
        let action = address.action.clone();
        SearchResult {
            kind: ResourceKind::Action,
            operation_kind: descriptor.kind,
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "directory{ID_SEPARATOR}{peer_id}{ID_SEPARATOR}{revision}{ID_SEPARATOR}{resource}{ID_SEPARATOR}{action}"
            ),
            title: descriptor.action_label.clone(),
            subtitle: descriptor
                .description
                .clone()
                .or_else(|| Some(format!("{} · {}", descriptor.resource_label, root.app_id))),
            badge: None,
            score,
            params,
            contract: Some(contract),
            on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
            on_invoke: None,
            on_begin_invoke: Some(std::rc::Rc::new(move |_, _, _, args| {
                let argument = if typed {
                    let mut values = args.to_json();
                    values.remove("confirm");
                    values.remove("confirmation_phrase");
                    InvokeArgument::Json(values)
                } else {
                    args.text("argument").map_or(InvokeArgument::None, |text| {
                        InvokeArgument::Text(text.to_owned())
                    })
                };
                let confirmation = if args.boolean("confirm") == Some(true) {
                    Some(Confirmation::Confirmed)
                } else {
                    args.text("confirmation_phrase")
                        .map(|value| Confirmation::Phrase(value.to_owned()))
                };
                // The same non-blocking door, confirmation check and
                // idempotent ledger every other caller uses; the owner
                // re-resolves the row at `revision` before anything starts.
                let admission = action_sessions
                    .open(&cluster_id, Some(&actor))
                    .map_err(ActionError::Refused)?
                    .invoke_job(InvokeRequest {
                        on_behalf_of: None,
                        invocation: tilo_actions::InvocationId::new(),
                        target: RemoteActionTarget {
                            provider: peer_id.to_string(),
                            address: address.clone(),
                        },
                        argument,
                        context: None,
                        confirmation,
                        revision: Some(revision.clone()),
                        fresh_binding: None,
                        wait_ms: None,
                    });
                let ticket = directory_start_ticket(
                    admission,
                    Rc::clone(&invalidations),
                    cache_key.clone(),
                    deadline_after(directory_start_timeout()),
                );
                Ok(InvocationDisposition::Pending(ticket))
            })),
        }
    }

    fn service_action_result(
        &self,
        room: &str,
        revision: &str,
        row: &CatalogueRow,
        needle: &str,
        actor: &str,
    ) -> Option<SearchResult> {
        let descriptor = row.action.as_ref()?;
        let score = crate::shell::search::remote_score(
            needle,
            &descriptor.action_label,
            &descriptor.resource_label,
        );
        let contract = descriptor.contract.clone();
        let mut params = descriptor.params.clone();
        push_confirmation_params(&mut params, &contract.advertised_confirmation());
        let provider = row.provider.clone();
        let app_id = row.app_id.clone();
        let address = descriptor.address.clone();
        let permission = Self::permission_id(descriptor.contract.permission);
        if [
            provider.as_str(),
            revision,
            app_id.as_str(),
            address.resource.as_str(),
            address.action.as_str(),
        ]
        .into_iter()
        .any(|field| field.is_empty() || field.contains(ID_SEPARATOR))
        {
            return None;
        }
        let room = room.to_string();
        let revision = revision.to_string();
        let actor = actor.to_string();
        let needle = needle.to_string();
        let invalidations = Rc::clone(&self.service_invalidations);
        let cache_key = (room.clone(), actor.clone(), needle);
        let resource = address.resource.clone();
        let action = address.action.clone();
        let start = Rc::new(ServiceStart {
            action_sessions: self.action_sessions.clone(),
            provider: provider.clone(),
            room,
            address,
            actor,
            revision: revision.clone(),
            typed_arguments: !descriptor.params.is_empty(),
        });
        Some(SearchResult {
            kind: ResourceKind::Action,
            operation_kind: descriptor.kind,
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "service{ID_SEPARATOR}{provider}{ID_SEPARATOR}{revision}{ID_SEPARATOR}{app_id}{ID_SEPARATOR}{permission}{ID_SEPARATOR}{resource}{ID_SEPARATOR}{action}"
            ),
            title: descriptor.action_label.clone(),
            subtitle: descriptor
                .description
                .clone()
                .or_else(|| Some(format!("{} · {app_id}", descriptor.resource_label))),
            badge: None,
            score,
            params,
            contract: Some(contract),
            on_open: Rc::new(|_, _, _, _| Ok(())),
            on_invoke: None,
            on_begin_invoke: Some(Rc::new(move |_, _, _, args| {
                let admission = start.begin(args)?;
                let refused_invalidations = Rc::clone(&invalidations);
                let refused_key = cache_key.clone();
                let timed_out_invalidations = Rc::clone(&invalidations);
                let timed_out_key = cache_key.clone();
                Ok(InvocationDisposition::Pending(admission_ticket(
                    admission,
                    deadline_after(directory_start_timeout()),
                    move |error| {
                        if invalidates_directory_cache(error) {
                            refused_invalidations
                                .borrow_mut()
                                .insert(refused_key.clone());
                        }
                        ActionError::Refused(format!("workspace action refused: {error}"))
                    },
                    move || {
                        timed_out_invalidations
                            .borrow_mut()
                            .insert(timed_out_key.clone());
                        ActionError::Refused("workspace action admission timed out".into())
                    },
                )))
            })),
        })
    }

    fn directory_resource_result(
        root: &ActionDirectoryRoot,
        row: &CatalogueRow,
        needle: &str,
    ) -> SearchResult {
        let presentation = row
            .resource
            .presentation
            .clone()
            .filter(tilo_actions::ResourcePresentation::is_valid);
        // The app's own index found this row for the needle.
        let rank = crate::shell::search::remote_score(
            needle,
            &row.resource.label,
            row.resource.id.as_str(),
        );
        let scope = crate::shell::chrome_state::SearchResourceScope {
            peer_id: root.peer_id,
            cluster_id: root.cluster_id.clone(),
            app_id: root.app_id.clone(),
            resource: row.resource.id.clone(),
            label: row.resource.label.clone(),
            kind: row.resource.kind.clone(),
        };
        let title = row.resource.label.clone();
        SearchResult {
            operation_kind: tilo_actions::OperationKind::View,
            kind: ResourceKind::Custom(if row.resource.kind.is_empty() {
                "Resources".into()
            } else {
                row.resource.kind.clone()
            }),
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "directory-resource{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
                root.peer_id, row.resource.id
            ),
            title,
            subtitle: Some(
                presentation
                    .as_ref()
                    .and_then(|value| value.detail.as_deref())
                    .map_or_else(
                        || {
                            t_args(
                                "search-app-resource-contains",
                                &[("app", root.app_id.as_str().into())],
                            )
                        },
                        |detail| format!("{} · {detail}", root.app_id),
                    ),
            ),
            badge: presentation.and_then(|value| value.badge),
            score: rank,
            params: Vec::new(),
            contract: None,
            on_open: Rc::new(move |_, _, chrome, _| {
                let mut state = chrome.borrow_mut();
                state.search_resource_scope = Some(scope.clone());
                state.bump();
                Ok(())
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }

    fn service_action_hierarchy(&self, id: &str) -> Option<ActionHierarchy> {
        let mut parts = id.split(ID_SEPARATOR);
        (parts.next()? == "service").then_some(())?;
        let provider = parts.next()?;
        let revision = parts.next()?;
        let app = parts.next()?;
        let permission = Self::permission_from_id(parts.next()?)?;
        let resource = parts.next()?;
        let operation = parts.next()?;
        if parts.next().is_some()
            || revision.is_empty()
            || resource.is_empty()
            || operation.is_empty()
        {
            return None;
        }
        let (room, row) = self
            .service_pages
            .borrow()
            .iter()
            .find_map(|((room, _, _), page)| {
                if page.revision != revision
                    || tilo_actions::remote::workspace_provider_room(provider)
                        != Some(room.as_str())
                {
                    return None;
                }
                page.rows.iter().find_map(|row| {
                    let descriptor = row.action.as_ref()?;
                    (row.provider == provider
                        && row.app_id == app
                        && descriptor.contract.permission == permission
                        && descriptor.address.resource == resource
                        && descriptor.address.action == operation)
                        .then(|| (room.clone(), row.clone()))
                })
            })?;
        let descriptor = row.action?;
        let semantic_identity = descriptor
            .semantic_identity
            .filter(|identity| identity.is_valid() && identity.kind == descriptor.resource_kind);
        Some(ActionHierarchy {
            application: row.app_id,
            room,
            record: String::new(),
            resource: resource.into(),
            operation: operation.into(),
            semantic_identity,
        })
    }

    fn resolve_service_action(&self, id: &str, ctx: &SearchContext<'_>) -> Option<SearchResult> {
        let mut fields = id.split(ID_SEPARATOR);
        (fields.next()? == "service").then_some(())?;
        let provider = fields.next()?;
        let revision = fields.next()?;
        let app = fields.next()?;
        let permission = Self::permission_from_id(fields.next()?)?;
        let resource = fields.next()?;
        let action = fields.next()?;
        if fields.next().is_some() || ctx.chrome.search_resource_scope.is_some() {
            return None;
        }
        self.apply_service_invalidations();
        self.poll_directory_pages();
        let workspace_room = {
            let tree = ctx.chrome.tree();
            let room = tree.desktop.active_workspace()?.room;
            drop(tree);
            room
        };
        let room = workspace_room.to_string();
        let actor = ctx.chrome.local_user_for_room(&workspace_room);
        if tilo_actions::remote::workspace_provider_room(provider) != Some(room.as_str()) {
            return None;
        }
        let cached = self.service_pages.borrow().iter().find_map(
            |((cached_room, cached_actor, needle), page)| {
                if cached_room != &room || cached_actor != &actor || page.revision != revision {
                    return None;
                }
                page.rows.iter().find_map(|row| {
                    let descriptor = row.action.as_ref()?;
                    (row.provider == provider
                        && row.app_id == app
                        && descriptor.contract.permission == permission
                        && descriptor.address.resource == resource
                        && descriptor.address.action == action)
                        .then(|| (row.clone(), needle.clone()))
                })
            },
        )?;
        self.service_action_result(&room, revision, &cached.0, &cached.1, &actor)
            .filter(|result| result.id == id)
    }

    fn resolve_job_control(&self, id: &str, ctx: &SearchContext<'_>) -> Option<SearchResult> {
        self.job_control_page(ctx, job_control_target(id)?, None, SearchBudget::PALETTE)
            .results
            .into_iter()
            .find(|result| result.id == id)
    }
}

impl SearchProvider for LiveAppActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Application resources and actions"
    }

    fn display_name(&self) -> String {
        t("search-provider-app-actions")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.resource_kinds
    }

    fn action_hierarchy(&self, id: &str) -> Option<ActionHierarchy> {
        let mut parts = id.split(ID_SEPARATOR);
        let kind = parts.next()?;
        if kind == "service" {
            return self.service_action_hierarchy(id);
        }
        if kind == "directory" {
            let peer = parts.next()?.parse::<u64>().ok()?;
            let revision = parts.next()?;
            let resource = parts.next()?;
            let operation = parts.next()?;
            if parts.next().is_some()
                || revision.is_empty()
                || resource.is_empty()
                || operation.is_empty()
            {
                return None;
            }
            // Exact lookup in the broker's retained sorted roots: bounded even
            // when the peer is beyond the first root page. Peer zero is valid.
            // A disconnected peer must not inherit its next neighbor's route.
            let root = self
                .broker
                .state()
                .action_directory_root_page(peer.checked_sub(1), 1)
                .roots
                .into_iter()
                .find(|root| root.peer_id == peer)?;
            return Some(ActionHierarchy {
                application: root.app_id,
                room: root.cluster_id,
                record: root.record_id,
                resource: resource.into(),
                operation: operation.into(),
                semantic_identity: self
                    .directory_semantic_identity(peer, revision, resource, operation),
            });
        }
        let (application, room, record, resource, operation) = Self::hierarchy(id)?;
        Some(ActionHierarchy {
            application: application.into(),
            room: room.into(),
            record: record.into(),
            resource: resource.into(),
            operation: operation.into(),
            semantic_identity: self
                .surface_snapshot
                .borrow()
                .as_ref()
                .filter(|snapshot| snapshot.source_generation == self.broker.surfaces_generation())
                .and_then(|snapshot| {
                    snapshot
                        .entries
                        .iter()
                        .find(|(surface, action)| Self::result_id(surface, action) == id)
                        .and_then(|(_, action)| {
                            action
                                .semantic_identity
                                .as_ref()
                                .filter(|identity| {
                                    identity.is_valid() && identity.kind == action.resource_kind
                                })
                                .cloned()
                        })
                }),
        })
    }

    fn available_in_active_app_scope(&self) -> bool {
        // The provider builds a separate active-app projection filtered by
        // room and app id. It complements the primary content provider that
        // owns the tab; excluding it would hide the app's canonical actions
        // precisely in the scope intended to expose them.
        true
    }

    fn owns_tab(&self, tab_id: u64, chrome: &crate::shell::chrome_state::ChromeState) -> bool {
        chrome.active_tab_id() == Some(tab_id)
            && chrome.active_app_id().is_some_and(|active| {
                self.broker
                    .surfaces()
                    .iter()
                    .any(|surface| active.eq_ignore_ascii_case(&surface.app_id))
            })
    }

    fn proximity(
        &self,
        result: &SearchResult,
        chrome: &crate::shell::chrome_state::ChromeState,
    ) -> SearchProximity {
        let current_room = chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        if Self::hierarchy(&result.id)
            .is_some_and(|(_, cluster, _, _, _)| current_room.as_deref() == Some(cluster))
        {
            return SearchProximity::CurrentWorkspace;
        }
        // Directory result ids are intentionally opaque and omit route data,
        // but the provider admits those rows in Workspace scope only after
        // matching their authenticated root's cluster to `current_room`.
        if chrome.search_scope == crate::shell::chrome_state::SearchScope::Workspace
            && (result.id.starts_with("directory\u{1f}") || result.id.starts_with("service\u{1f}"))
        {
            return SearchProximity::CurrentWorkspace;
        }
        SearchProximity::Global
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the query constructs paired palette/headless callbacks from one captured action contract; splitting them would duplicate or desynchronise confirmation, launch, and job lifecycle state"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the broker handle is cloned once per callback; each clone moves into its own closure"
    )]
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if self.projection.get() == QueryProjection::All {
            // Direct compatibility callers do not have a registry refresh
            // pass. The interactive palette performs this before it enters
            // `query_page`, so the canonical paging path never indexes a
            // cold action corpus synchronously.
            let _ = self.search_revision(&ctx);
            return self.query_page(ctx, None, SearchBudget::PALETTE).results;
        }
        let needle = ctx.needle.trim().to_lowercase();
        let actor = ctx.chrome.active_local_user();
        let mut results = Vec::new();
        if self.projection.get() != QueryProjection::LocalOnly {
            self.set_directory_context(&ctx, &needle);
            self.apply_directory_invalidations();
            self.apply_service_invalidations();
            self.poll_directory_pages();
        }
        if self.projection.get() != QueryProjection::LocalOnly {
            for root in self
                .directory_roots(&ctx)
                .0
                .into_iter()
                .take(MAX_DIRECTORY_ROOTS_PER_QUERY)
            {
                if let Some(resource_scope) = &ctx.chrome.search_resource_scope
                    && (root.peer_id != resource_scope.peer_id
                        || root.cluster_id != resource_scope.cluster_id
                        || root.app_id != resource_scope.app_id)
                {
                    continue;
                }
                if !matches!(
                    ctx.effective_scope(),
                    crate::shell::chrome_state::SearchScope::All
                ) && ctx
                    .chrome
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room.to_string())
                    != Some(root.cluster_id.clone())
                {
                    continue;
                }
                // A directory is scoped to the room that certified its route.
                // Never reuse the active-room principal for another room: an
                // actor must be derived from that exact room or this root fails
                // closed.
                let root_actor = ctx
                    .chrome
                    .tree()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room.to_string() == root.cluster_id)
                    .map(|workspace| ctx.chrome.local_user_for_room(&workspace.room));
                let Some(root_actor) = root_actor else {
                    continue;
                };
                self.request_directory_page(
                    root.peer_id,
                    &root.cluster_id,
                    &root_actor,
                    &needle,
                    &[],
                    ctx.chrome
                        .search_resource_scope
                        .as_ref()
                        .map_or("", |scope| scope.resource.as_str()),
                );
                let page = self
                    .directory_pages
                    .borrow()
                    .get(&(root.peer_id, root_actor.clone(), needle.clone(), Vec::new()))
                    .cloned();
                if let Some(page) = page {
                    for row in page.rows {
                        if let Some(descriptor) = row.action
                            && (ctx.include_programmatic || !descriptor.contract.is_programmatic())
                        {
                            results.push(self.directory_action_result(
                                &root,
                                &page.revision,
                                &descriptor,
                                &needle,
                                &root_actor,
                                &[],
                            ));
                        }
                    }
                }
            }
        }
        if self.projection.get() != QueryProjection::LocalOnly
            && (needle.is_empty() || "jobs status progress".contains(&needle))
        {
            for (id, update) in self.broker.statuses(&actor) {
                let state = job_state(&update);
                let status_broker = self.broker.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Custom("action-job".into()),
                    operation_kind: tilo_actions::OperationKind::View,
                    provider_id: PROVIDER_ID.into(),
                    id: job_status_id(id),
                    title: t_args("search-app-job", &[("id", id.to_string().into())]),
                    subtitle: Some(job_state_label(state)),
                    badge: None,
                    score: -10,
                    params: Vec::new(),
                    contract: None,
                    on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
                    on_invoke: Some(std::rc::Rc::new(move |_, _, _, _| {
                        let update = status_broker
                            .status(id)
                            .ok_or_else(|| ActionError::Refused("unknown invocation id".into()))?;
                        Ok(ActionReceipt {
                            broker_invocation_id: Some(id),
                            update,
                        })
                    })),
                    on_begin_invoke: None,
                });
                if self.broker.can_cancel(id, &actor) {
                    let broker = self.broker.clone();
                    let actor = actor.clone();
                    let invoke_broker = broker.clone();
                    let invoke_actor = actor.clone();
                    results.push(SearchResult {
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        provider_id: PROVIDER_ID.into(),
                        id: job_cancel_id(id),
                        title: t_args("search-app-job-cancel", &[("id", id.to_string().into())]),
                        subtitle: Some(t("search-app-job-cancel-detail")),
                        badge: None,
                        score: 5,
                        params: Vec::new(),
                        contract: None,
                        on_open: std::rc::Rc::new(move |_, _, _, _| {
                            broker.cancel(id, &actor).map_err(ActionError::Refused)
                        }),
                        on_invoke: Some(std::rc::Rc::new(move |_, _, _, _| {
                            invoke_broker
                                .cancel(id, &invoke_actor)
                                .map_err(ActionError::Refused)?;
                            let update = invoke_broker.status(id).ok_or_else(|| {
                                ActionError::Refused("unknown invocation id".into())
                            })?;
                            Ok(ActionReceipt {
                                broker_invocation_id: Some(id),
                                update,
                            })
                        })),
                        on_begin_invoke: None,
                    });
                }
            }
        }
        if self.projection.get() != QueryProjection::All {
            for (surface, owned_action, rank) in
                self.local_action_candidates(&ctx, SearchBudget::PALETTE.max_results)
            {
                // The index is a retained search accelerator, not a live
                // publication. A closed/replaced window can disappear while
                // its replacement index is still building. Resolve only this
                // bounded candidate, preserving its disclosed authority, and
                // construct metadata and callbacks from the selected source.
                let id = Self::result_id(&surface, &owned_action);
                let Some((surface, owned_action)) = self.broker.published_action(
                    &surface.route,
                    &owned_action.resource,
                    &owned_action.action,
                ) else {
                    continue;
                };
                if Self::result_id(&surface, &owned_action) != id {
                    continue;
                }
                let action = &owned_action;
                let expected_person = ctx
                    .chrome
                    .tree()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room.to_string() == surface.route.cluster_id)
                    .map(|workspace| ctx.chrome.local_user_for_room(&workspace.room));
                let Some(expected_person) = expected_person else {
                    continue;
                };
                let unavailable_reason = match &action.contract.availability {
                    tilo_actions::Availability::Available => None,
                    tilo_actions::Availability::Unavailable { reason, .. } => Some(reason.clone()),
                };
                let score = crate::shell::search::provider_score(rank);
                let params = Self::action_params(action);
                let surface_live = surface.live;
                let start = Rc::new(self.local_start(&surface, action, expected_person));
                // Availability on a retained launchable descriptor is only a
                // last-known hint. The host launches the app first and lets
                // the freshly published surface decide; otherwise an app
                // closed while temporarily busy could never be opened
                // through that action again.
                let unavailable = unavailable_reason.clone().filter(|_| surface_live);
                let open_start = Rc::clone(&start);
                let open_unavailable = unavailable.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Action,
                    operation_kind: action.operation_kind,
                    provider_id: PROVIDER_ID.to_string(),
                    id: Self::result_id(&surface, action),
                    title: action.action_label.clone(),
                    subtitle: Some(unavailable_reason.clone().unwrap_or_else(|| {
                        action.description.clone().unwrap_or_else(|| {
                            if surface_live {
                                format!("{} · {}", action.resource_label, surface.app_id)
                            } else {
                                t_args(
                                    "search-app-action-launches",
                                    &[
                                        ("resource", action.resource_label.as_str().into()),
                                        ("app", surface.app_id.as_str().into()),
                                    ],
                                )
                            }
                        })
                    })),
                    badge: None,
                    score,
                    params,
                    contract: Some(action.contract.clone()),
                    on_open: Rc::new(move |_doc, _root, _chrome, args| {
                        if let Some(reason) = &open_unavailable {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let mut admission = open_start.begin(args);
                        match admission.poll_ready() {
                            PendingPoll::Ready(result) => result
                                .map(|(job, _)| {
                                    tracing::debug!(job, "application action accepted");
                                })
                                .map_err(|error| ActionError::Refused(error.to_string())),
                            PendingPoll::Waiting => {
                                // Launching a closed app first: the host
                                // delivers once the app is up. Keep listening
                                // so the delivery is not abandoned.
                                admission.then(|answer| match answer {
                                    Some(Ok((job, _))) => {
                                        tracing::debug!(job, "launched application action accepted");
                                    }
                                    Some(Err(error)) => {
                                        tracing::warn!(%error, "launched application action was refused");
                                    }
                                    None => {}
                                });
                                Ok(())
                            }
                            PendingPoll::Abandoned => Err(ActionError::Refused(
                                "the action's admission was unavailable".into(),
                            )),
                        }
                    }),
                    on_invoke: None,
                    on_begin_invoke: Some(Rc::new(move |_doc, _root, _chrome, args| {
                        if let Some(reason) = &unavailable {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let ticket = admission_ticket(
                            start.begin(args),
                            deadline_after(LOCAL_ADMISSION_TIMEOUT),
                            |error| ActionError::Refused(error.to_string()),
                            || ActionError::Refused("the action was not admitted in time".into()),
                        );
                        Ok(match ticket()? {
                            Some(receipt) => InvocationDisposition::Accepted(receipt),
                            None => InvocationDisposition::Pending(ticket),
                        })
                    })),
                });
            }
        }
        results
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        // A receipt read or cancellation names one actor-owned job. It must
        // not depend on that job still being inside the bounded browse preview.
        if job_control_target(id).is_some() {
            return self.resolve_job_control(id, &ctx);
        }
        let mut fields = id.split(ID_SEPARATOR);
        let kind = fields.next();
        if kind == Some("service") {
            return self.resolve_service_action(id, &ctx);
        }
        if kind != Some("directory") {
            if Self::result_parts(id).is_none() {
                return self
                    .query_page(ctx, None, SearchBudget::PALETTE)
                    .results
                    .into_iter()
                    .find(|result| result.id == id);
            }
            if ctx.chrome.search_resource_scope.is_some() {
                return None;
            }
            let (app, _, cluster, record, resource, action) = Self::result_parts(id)?;
            let scope = Self::build_scope(&ctx);
            let in_scope = match scope.scope {
                crate::shell::chrome_state::SearchScope::All => true,
                crate::shell::chrome_state::SearchScope::Workspace => {
                    scope.room.as_deref() == Some(cluster)
                }
                crate::shell::chrome_state::SearchScope::ActiveApp => {
                    scope.room.as_deref() == Some(cluster)
                        && scope
                            .app
                            .as_deref()
                            .is_some_and(|active| active.eq_ignore_ascii_case(app))
                }
            };
            if !in_scope {
                return None;
            }
            let route = crate::shell::collab_broker::CrdtRoute {
                app_id: app.into(),
                cluster_id: cluster.into(),
                record_id: record.into(),
            };
            let candidate = self.broker.published_action(&route, resource, action)?;
            // The opaque address includes its permission requirement. Never
            // use an old address with a newly changed contract or route.
            if Self::result_id(&candidate.0, &candidate.1) != id {
                return None;
            }
            return self.materialize_local_action(&ctx, candidate.0, candidate.1, 0);
        }
        let peer_id = fields.next()?.parse::<u64>().ok()?;
        let revision = fields.next()?;
        let resource = fields.next()?;
        let action = fields.next()?;
        if fields.next().is_some() {
            return None;
        }

        self.apply_directory_invalidations();
        self.apply_service_invalidations();
        self.poll_directory_pages();
        let root = self
            .directory_roots(&ctx)
            .0
            .into_iter()
            .find(|root| root.peer_id == peer_id)?;
        if !matches!(
            ctx.effective_scope(),
            crate::shell::chrome_state::SearchScope::All
        ) && ctx
            .chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
            != Some(root.cluster_id.clone())
        {
            return None;
        }
        let actor = ctx
            .chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == root.cluster_id)
            .map(|workspace| ctx.chrome.local_user_for_room(&workspace.room))?;
        let cached = self.directory_pages.borrow().iter().find_map(
            |((cached_peer, cached_actor, needle, cursor), page)| {
                if *cached_peer != peer_id || cached_actor != &actor || page.revision != revision {
                    return None;
                }
                page.rows.iter().find_map(|row| {
                    let descriptor = row.action.as_ref()?;
                    (descriptor.address.resource == resource && descriptor.address.action == action)
                        .then(|| (descriptor.clone(), needle.clone(), cursor.clone()))
                })
            },
        )?;
        Some(self.directory_action_result(&root, revision, &cached.0, &cached.1, &actor, &cached.2))
            .filter(|result| result.id == id)
    }

    fn refresh_search_index(&self, ctx: &SearchContext<'_>) {
        self.refresh_local_snapshot(ctx);
        self.poll_directory_pages();
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        let scope = Self::surface_scope(ctx);
        let source_generation = self.broker.surfaces_generation();
        let ready = self
            .surface_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.source_generation == source_generation && snapshot.scope == scope
            });
        format!(
            "{}:{source_generation}:{}:{}:{}:{}:{}",
            if ready { "ready" } else { "indexing" },
            self.directory_epoch.load(Ordering::Relaxed),
            // Listing publishers is a memory read; it never asks an app.
            self.directory_revision(ctx.chrome),
            self.broker.statuses_generation(),
            ctx.chrome.active_local_user(),
            ctx.include_programmatic,
        )
    }

    /// Settling while the local index for this scope is not built from the
    /// current publications, or while an application directory or workspace
    /// service page is still owed. Each can add rows to the current listing.
    fn index_settling(&self, ctx: &SearchContext<'_>) -> bool {
        let scope = Self::surface_scope(ctx);
        let source_generation = self.broker.surfaces_generation();
        let ready = self
            .surface_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.source_generation == source_generation && snapshot.scope == scope
            });
        !ready
            || !self.directory_pending.borrow().is_empty()
            || !self.service_pending.borrow().is_empty()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the page owns one ordered budget across auxiliary and local retained projections, so split helpers would obscure cursor handoff"
    )]
    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if budget.max_results == 0
            || budget.max_work == 0
            || (!ctx.wanted.is_empty()
                && !ctx.wanted.contains(&ResourceKind::Action)
                && !Self::wants_resource_rows(ctx.wanted))
        {
            return SearchPage::exhausted(self.search_revision(&ctx), Vec::new());
        }
        if let Some(target) = job_control_target(ctx.needle) {
            return self.job_control_page(&ctx, target, cursor, budget);
        }
        let needle = ctx.needle.trim().to_lowercase();
        self.set_directory_context(&ctx, &needle);
        self.apply_directory_invalidations();
        self.apply_service_invalidations();
        self.poll_directory_pages();
        let revision = self.search_revision(&ctx);
        let scope = Self::surface_scope(&ctx);
        let (stage, after, resume) = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => {
                let Some(token) = cursor.token.strip_prefix("live:") else {
                    return SearchPage::stale(revision);
                };
                let mut parts = token.splitn(4, ID_SEPARATOR);
                let (Some(stage), Some(after), Some(query), Some(tail)) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return SearchPage::stale(revision);
                };
                let (cursor_scope, encoded_resume) = if stage == "aux" {
                    let Some((encoded_resume, cursor_scope)) = tail.split_once(ID_SEPARATOR) else {
                        return SearchPage::stale(revision);
                    };
                    (cursor_scope, Some(encoded_resume))
                } else {
                    (tail, None)
                };
                if query != needle || cursor_scope != scope || !matches!(stage, "aux" | "local") {
                    return SearchPage::stale(revision);
                }
                let (after, resume) = if stage == "aux" {
                    let Some(position) = after.parse::<usize>().ok() else {
                        return SearchPage::stale(revision);
                    };
                    let Some(resume) = encoded_resume.and_then(Self::decode_auxiliary_resume)
                    else {
                        return SearchPage::stale(revision);
                    };
                    (position, resume)
                } else if stage == "local" && after == "-" {
                    (
                        usize::MAX,
                        AuxiliaryResume {
                            root_after: None,
                            directory: None,
                        },
                    )
                } else {
                    let Some(after) = after.parse::<usize>().ok() else {
                        return SearchPage::stale(revision);
                    };
                    (
                        after,
                        AuxiliaryResume {
                            root_after: None,
                            directory: None,
                        },
                    )
                };
                (stage, after, resume)
            }
            None => (
                "aux",
                0,
                AuxiliaryResume {
                    root_after: None,
                    directory: None,
                },
            ),
        };

        // Build the local retained index only on a source-generation/scope
        // change. The auxiliary projection is likewise retained for this
        // query/revision and contains a finite front stage (jobs, then cached
        // remote directory rows). No palette page clones a full local corpus.
        // The registry invoked `search_revision` before this bounded page.
        // Do not fall back to cloning broker surfaces here on a cold source.
        self.prepare_auxiliary_snapshot(&ctx, &needle, &revision, budget.max_work, &resume);
        let mut bytes = 0usize;
        let mut results = Vec::new();
        let mut work = 0usize;
        if stage == "aux" {
            let snapshot = self.auxiliary_snapshot.borrow();
            let Some(snapshot) = snapshot.as_ref() else {
                return SearchPage::exhausted(revision, Vec::new());
            };
            let mut position = after;
            while position < snapshot.entries.len()
                && work < budget.max_work
                && results.len() < budget.max_results
            {
                let Some(entry) = snapshot.entries.get(position) else {
                    break;
                };
                let row = match entry {
                    AuxiliaryEntry::Job {
                        id,
                        state,
                        cancel,
                        actor,
                    } => {
                        if !ctx.wanted.is_empty() && !ctx.wanted.contains(&ResourceKind::Action) {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        self.job_result(*id, state, *cancel, actor)
                    }
                    AuxiliaryEntry::Directory {
                        root,
                        revision,
                        descriptor,
                        actor,
                        cursor,
                    } => {
                        if (descriptor.contract.is_programmatic() && !ctx.include_programmatic)
                            || (!ctx.wanted.is_empty()
                                && !ctx.wanted.contains(&ResourceKind::Action))
                        {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        self.directory_action_result(
                            root, revision, descriptor, &needle, actor, cursor,
                        )
                    }
                    AuxiliaryEntry::Service {
                        room,
                        revision,
                        row,
                        actor,
                    } => {
                        if row.action.as_ref().is_some_and(|descriptor| {
                            descriptor.contract.is_programmatic() && !ctx.include_programmatic
                        }) || (!ctx.wanted.is_empty()
                            && !ctx.wanted.contains(&ResourceKind::Action))
                        {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        let Some(result) =
                            self.service_action_result(room, revision, row, &needle, actor)
                        else {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        };
                        result
                    }
                    AuxiliaryEntry::Resource { root, row } => {
                        if !Self::wants_resource_rows(ctx.wanted) {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        Self::directory_resource_result(root, row, &needle)
                    }
                };
                position = position.saturating_add(1);
                work = work.saturating_add(1);
                let row_bytes = row
                    .title
                    .len()
                    .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
                if bytes.saturating_add(row_bytes) <= budget.max_bytes {
                    bytes = bytes.saturating_add(row_bytes);
                    results.push(row);
                }
            }
            if position < snapshot.entries.len() {
                return Self::live_aux_page(
                    revision,
                    results,
                    position,
                    &needle,
                    &scope,
                    &snapshot.resume,
                );
            }
            if let Some(next) = snapshot.pending_directory.first() {
                // Start exactly one next remote page only after every cached
                // row before it has been considered. The continuation keeps
                // its opaque remote cursor while that pull is pending.  Once
                // the bounded cache evicts the preceding page, the public
                // cursor can still rebuild directly from this continuation
                // rather than replaying or losing already-returned rows.
                self.request_directory_page(
                    next.root.peer_id,
                    &next.root.cluster_id,
                    &next.actor,
                    &needle,
                    &next.cursor,
                    ctx.chrome
                        .search_resource_scope
                        .as_ref()
                        .map_or("", |scope| scope.resource.as_str()),
                );
                return Self::live_aux_page(
                    revision,
                    results,
                    position,
                    &needle,
                    &scope,
                    &AuxiliaryResume {
                        root_after: snapshot.resume.root_after,
                        directory: Some(next.clone()),
                    },
                );
            }
            if let Some(next) = snapshot.next_resume.clone() {
                return Self::live_aux_page(revision, results, 0, &needle, &scope, &next);
            }
            if results.len() == budget.max_results || work == budget.max_work {
                return Self::live_page(revision, results, "local", usize::MAX, &needle, &scope);
            }
        }

        if ctx.chrome.search_resource_scope.is_some() {
            return SearchPage::exhausted(revision, results);
        }

        let local_offset = if stage == "local" && after != usize::MAX {
            after
        } else {
            0
        };
        let local_limit = budget.max_results.saturating_sub(results.len());
        let local_work = budget.max_work.saturating_sub(work);
        if local_limit == 0 || local_work == 0 {
            return Self::live_page(revision, results, "local", local_offset, &needle, &scope);
        }
        let (found, candidates) = {
            let snapshot = self.surface_snapshot.borrow();
            let Some(snapshot) = snapshot.as_ref() else {
                return SearchPage::exhausted(revision, results);
            };
            // A completed projection may belong to the preceding palette scope
            // while its replacement builds. Never publish those rows in the
            // new scope, even when their current permission is unchanged.
            if snapshot.scope != scope {
                return SearchPage::exhausted(revision, results);
            }
            let Some(index) = (if ctx.include_programmatic {
                &snapshot.machine_index
            } else {
                &snapshot.index
            }) else {
                return SearchPage::exhausted(revision, results);
            };
            let found = index.search(
                &needle,
                Page {
                    offset: local_offset,
                    limit: local_work.min(local_limit),
                },
            );
            let candidates = found
                .hits
                .iter()
                .map(|hit| {
                    let (surface, action) = usize::try_from(hit.key)
                        .ok()
                        .and_then(|at| snapshot.entries.get(at).cloned())?;
                    Some((surface, action, hit.rank.unwrap_or(0)))
                })
                .collect::<Vec<_>>();
            (found, candidates)
        };
        let mut considered = 0usize;
        for candidate in candidates {
            let row = candidate
                .filter(|(surface, action, _)| {
                    !self.has_disclosed_directory_alias(&ctx, surface, action)
                })
                .and_then(|(surface, action, rank)| {
                    self.materialize_local_action(&ctx, surface, action, rank)
                })
                .filter(|row| {
                    ctx.include_programmatic
                        || !row
                            .contract
                            .as_ref()
                            .is_some_and(tilo_actions::ActionContract::is_programmatic)
                });
            if let Some(row) = row {
                let row_bytes = row
                    .title
                    .len()
                    .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
                if bytes.saturating_add(row_bytes) > budget.max_bytes {
                    break;
                }
                bytes = bytes.saturating_add(row_bytes);
                results.push(row);
            }
            considered = considered.saturating_add(1);
        }
        if !found.more && considered == found.hits.len() {
            return SearchPage::exhausted(revision, results);
        }
        let next = local_offset.saturating_add(considered);
        Self::live_page(revision, results, "local", next, &needle, &scope)
    }
}

/// Index the fields a local action row ranks by, keyed by position.
fn index_actions(
    entries: &[(PublishedSurface, PublishedAction)],
    include_programmatic: bool,
) -> SearchIndex {
    let mut index = SearchIndex::new(Matching {
        spanning: true,
        typos: false,
    });
    index.replace_all(
        entries
            .iter()
            .enumerate()
            .filter(|(_, (_, action))| include_programmatic || !action.contract.is_programmatic())
            .map(|(at, (surface, action))| {
                Document::new(at as u64)
                    .field(action.action_label.as_str(), FieldWeight::TITLE)
                    .field(action.resource_label.as_str(), FieldWeight::SUBTITLE)
                    .field(action.action.as_str(), FieldWeight::IDENTITY)
                    .field(action.resource.as_str(), FieldWeight::IDENTITY)
                    .field(surface.app_id.as_str(), FieldWeight::METADATA)
            }),
    );
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index live app actions");
    }
    index
}

#[cfg(test)]
pub(crate) fn fixture_with_directory_page(
    broker: AppActionBroker,
    peer: u64,
    actor: String,
    page: CataloguePage,
) -> Rc<dyn SearchProvider> {
    let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker));
    provider
        .directory_pages
        .borrow_mut()
        .insert((peer, actor, String::new(), Vec::new()), page);
    provider
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::{
        AuxiliaryEntry, AuxiliaryResume, AuxiliarySnapshot, DIRECTORY_PAGE_REPLY_TIMEOUT,
        DirectoryCacheKey, ID_SEPARATOR, LiveAppActionsProvider, PROVIDER_ID, PendingDirectoryPage,
        SurfaceSnapshot, directory_start_ticket, index_actions, invalidates_directory_cache,
    };
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::collab_broker::CrdtRoute;
    use crate::shell::collab_broker_capnp::{
        ActionDirectoryRoot, PublishedAction, PublishedInput, PublishedSurface,
    };
    use crate::shell::search::{
        ResourceKind, SearchBudget, SearchContext, SearchProvider, SearchProximity, SearchResult,
    };
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use tilo_actions::remote::{
        ActionHostError, CataloguePage, CatalogueResource, CatalogueRow, pending,
    };

    fn context<'a>(
        needle: &'a str,
        chrome: &'a crate::shell::chrome_state::ChromeState,
        doc: &'a AutoCommit,
    ) -> SearchContext<'a> {
        SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome,
            doc,
            doc_root: &automerge::ROOT,
        }
    }

    fn chrome() -> crate::shell::chrome_state::Chrome {
        let mut workspace_tree = WorkspaceTree::default();
        workspace_tree
            .desktop
            .workspaces
            .push(Workspace::new(WorkspaceId(1), "Search fixture"));
        workspace_tree.desktop.active = WorkspaceId(1);
        let tree: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(workspace_tree);
        ChromeState::new_with_handle(tree)
    }

    fn succeeded(id: u64) -> tilo_actions::InvocationUpdate {
        tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
            output: tilo_actions::ActionOutput::default(),
        }
    }

    #[test]
    fn search_revision_opens_no_session_when_nothing_changed() {
        let provider = LiveAppActionsProvider::new_with_fixture_identity(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let chrome = chrome();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = context("", &chrome, &doc);
        let first = provider.search_revision(&ctx);
        assert_eq!(provider.search_revision(&ctx), first);
        assert_eq!(
            provider.action_sessions.opens(),
            0,
            "the palette tick reads stamps and counters, never a certified session"
        );
        // Listing the roots themselves still goes through each workspace's session.
        let _ = provider.directory_roots(&ctx);
        assert!(provider.action_sessions.opens() > 0);
        assert_eq!(provider.search_revision(&ctx), first);
    }

    #[test]
    fn job_receipts_resolve_beyond_the_preview_and_only_for_their_actor() {
        fn ctx<'a>(
            chrome: &'a crate::shell::chrome_state::ChromeState,
            wanted: &'a [ResourceKind],
            doc: &'a AutoCommit,
        ) -> SearchContext<'a> {
            SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted,
                chrome,
                doc,
                doc_root: &automerge::ROOT,
            }
        }

        let chrome = chrome();
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
        let doc = AutoCommit::new();
        let actor = chrome.borrow().active_local_user();
        let wanted = provider.resource_kinds().to_vec();
        let before = provider.search_revision(&ctx(&chrome.borrow(), &wanted, &doc));
        let preview_rows = u64::try_from(super::MAX_JOB_ROWS_PER_QUERY).unwrap();
        for id in 1..=preview_rows + 3 {
            broker.record_completion_for_test(id, &actor, succeeded(id));
        }
        broker.record_completion_for_test(99, "someone-else", succeeded(99));
        assert_ne!(
            provider.search_revision(&ctx(&chrome.borrow(), &wanted, &doc)),
            before,
            "a new receipt must invalidate the retained job projection"
        );
        for id in 1..=preview_rows + 3 {
            let row = provider
                .resolve(
                    &super::job_status_id(id),
                    ctx(&chrome.borrow(), &wanted, &doc),
                )
                .unwrap_or_else(|| panic!("the actor's job {id} must stay readable"));
            assert_eq!(row.id, super::job_status_id(id));
            assert_eq!(row.subtitle.as_deref(), Some("succeeded"));
        }
        assert!(
            provider
                .resolve(
                    &super::job_status_id(99),
                    ctx(&chrome.borrow(), &wanted, &doc)
                )
                .is_none(),
            "another actor's receipt is indistinguishable from an unknown one"
        );
        assert!(
            provider
                .resolve(
                    &super::job_cancel_id(1),
                    ctx(&chrome.borrow(), &wanted, &doc)
                )
                .is_none(),
            "a completed job offers no cancellation"
        );
    }

    #[test]
    fn action_catalog_routes_receipts_through_the_registered_job_kind() {
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

        let chrome = chrome();
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let actor = chrome.borrow().active_local_user();
        broker.record_completion_for_test(17, &actor, succeeded(17));
        broker.record_completion_for_test(18, "someone-else", succeeded(18));
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(LiveAppActionsProvider::new_with_fixture_identity(
            broker,
        )));
        let mut doc = AutoCommit::new();
        for (id, visible) in [(17, true), (18, false), (19, false)] {
            let resource = ResourceRef::new(PROVIDER_ID, super::job_status_id(id));
            let catalog = ActionCatalog::build_for_resource(
                &registry,
                &resource,
                crate::shell::chrome_state::SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &automerge::ROOT,
                &AllVisible,
            );
            let selected = catalog.select(&resource);
            assert_eq!(selected.is_some(), visible);
            if let Some(selected) = selected {
                let disposition = selected
                    .invoke_nullary(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &automerge::ROOT,
                        &chrome,
                    )
                    .unwrap();
                let crate::shell::search::InvocationDisposition::Accepted(receipt) = disposition
                else {
                    panic!("an existing receipt must resolve immediately");
                };
                assert_eq!(receipt.broker_invocation_id, Some(id));
                assert_eq!(receipt.update, succeeded(id));
            }
        }
    }

    fn exported(label: &str, action: &str) -> tilo_actions::ActionDescriptor {
        tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(format!("resource:{action}"), action),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            action_label: label.into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }

    #[test]
    fn hierarchy_projects_only_exact_retained_semantic_identity() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        broker.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
            peer_id: 9,
            app_id: "so.tilo.store".into(),
            cluster_id: "room".into(),
            record_id: "record".into(),
        }]);
        let provider = LiveAppActionsProvider::new(broker);
        let identity =
            tilo_actions::ResourceSemanticIdentity::new("store.app", "app:so.tilo.chat").unwrap();
        let mut descriptor = exported("Misleading label", "open");
        descriptor.resource_kind = "store.app".into();
        descriptor.semantic_identity = Some(identity.clone());
        let row = CatalogueRow {
            provider: "9".into(),
            app_id: "so.tilo.store".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: descriptor.address.resource.clone(),
                kind: "fixture".into(),
                label: "Not an identity".into(),
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            fresh_binding: None,
            visibility: None,
        };
        provider.directory_pages.borrow_mut().insert(
            (9, "actor".into(), String::new(), Vec::new()),
            page(vec![row.clone()], Vec::new()),
        );
        let id = ["directory", "9", "r1", "resource:open", "open"].join(&ID_SEPARATOR.to_string());
        assert_eq!(
            provider.action_hierarchy(&id).unwrap().semantic_identity,
            Some(identity)
        );
        assert!(
            provider
                .action_hierarchy(&id.replace("r1", "r0"))
                .unwrap()
                .semantic_identity
                .is_none()
        );
        let mut conflicting = row;
        conflicting.action.as_mut().unwrap().semantic_identity =
            tilo_actions::ResourceSemanticIdentity::new("store.app", "other");
        provider.directory_pages.borrow_mut().insert(
            (9, "actor".into(), "another-query".into(), Vec::new()),
            page(vec![conflicting], Vec::new()),
        );
        assert!(
            provider
                .action_hierarchy(&id)
                .unwrap()
                .semantic_identity
                .is_none()
        );
        provider.directory_pages.borrow_mut().clear();
        assert!(
            provider
                .action_hierarchy(&id)
                .unwrap()
                .semantic_identity
                .is_none()
        );
    }

    #[test]
    fn local_hierarchy_identity_requires_current_retained_descriptor() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new(broker.clone());
        let mut action = local_action("Display label is not identity", "open");
        action.resource_kind = "store.app".into();
        action.resource = "opaque-address".into();
        let identity =
            tilo_actions::ResourceSemanticIdentity::new("store.app", "app:so.tilo.chat").unwrap();
        action.semantic_identity = Some(identity.clone());
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.store".into(),
            route: CrdtRoute::app("so.tilo.store", "room"),
            actions: vec![action.clone()],
            live: true,
        };
        let entries = vec![(surface.clone(), action)];
        let id = LiveAppActionsProvider::result_id(&surface, surface.actions.first().unwrap());
        assert!(
            provider
                .action_hierarchy(&id)
                .unwrap()
                .semantic_identity
                .is_none()
        );
        *provider.surface_snapshot.borrow_mut() = Some(SurfaceSnapshot {
            source_generation: broker.surfaces_generation(),
            scope: String::new(),
            entries,
            index: None,
            machine_index: None,
        });
        assert_eq!(
            provider.action_hierarchy(&id).unwrap().semantic_identity,
            Some(identity)
        );
        broker.replace_published_surfaces_for_test(vec![surface]);
        assert!(
            provider
                .action_hierarchy(&id)
                .unwrap()
                .semantic_identity
                .is_none()
        );
    }

    /// A broker whose workspace authority is the chrome's own tree, so the
    /// action host offers that tree's rooms.
    fn broker_for(
        chrome: &crate::shell::chrome_state::Chrome,
    ) -> crate::shell::collab_broker::AppActionBroker {
        crate::shell::broker_services::test_broker_with_membership(chrome.borrow().tree_handle())
    }

    fn finish_local_index(provider: &LiveAppActionsProvider, ctx: &SearchContext<'_>) {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(3))
            .expect("the fixture deadline fits the monotonic clock");
        loop {
            // Complete the actual production worker without polling the
            // separately owed workspace-service response.
            provider.refresh_local_snapshot(ctx);
            if provider.surface_build.borrow().is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the real local index did not complete"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn workspace_service_listing_settles_only_after_current_authority_reply() {
        use crate::shell::search::{QueryArgs, SearchRegistry};
        use collab_types::{CapOverride, Effect, Scope, Subject, Verb};

        let chrome = chrome();
        let owner = chrome.borrow().active_local_user();
        chrome.borrow().tree_handle().write().desktop.workspaces[0].set_owner(owner.clone());
        let broker = broker_for(&chrome);
        let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker));
        let doc = AutoCommit::new();
        let mut registry = SearchRegistry::new();
        registry.register(provider.clone());

        for authority_changed in [false, true] {
            if authority_changed {
                let handle = chrome.borrow().tree_handle();
                let mut tree = handle.write();
                tree.desktop.workspaces[0].grant_capability(CapOverride::new(
                    Subject::person(owner.clone()),
                    Scope::App("so.tilo.chat".into()),
                    Effect::Restrict,
                    Verb::Read,
                ));
                // Native authority projection advances this cache boundary;
                // the signed projection itself is tested at its owner seam.
                tree.desktop.room_epoch = tree.desktop.room_epoch.wrapping_add(1);
            }
            let chrome_state = chrome.borrow();
            let ctx = SearchContext {
                query_scope: Some(crate::shell::chrome_state::SearchScope::All),
                ..context("profile.save", &chrome_state, &doc)
            };
            let args = QueryArgs {
                needle: "profile.save",
                scope: crate::shell::chrome_state::SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[ResourceKind::Action],
                chrome: &chrome_state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            let cold = provider.query_page(ctx, None, SearchBudget::PALETTE);
            assert!(
                cold.results
                    .iter()
                    .all(|row| !row.id.starts_with("service\u{1f}"))
            );
            finish_local_index(&provider, &ctx);
            assert!(provider.directory_pending.borrow().is_empty());
            assert!(!provider.service_pending.borrow().is_empty());
            assert!(
                registry.indexes_settling(&args),
                "an owed workspace-service reply must keep the real listing incomplete"
            );

            let completed = provider.query_page(ctx, None, SearchBudget::PALETTE);
            let row = completed
                .results
                .iter()
                .find(|row| row.id.ends_with("\u{1f}profile.save"))
                .expect("the actual broker offers the independent governance action");
            let hierarchy = provider.action_hierarchy(&row.id).unwrap();
            assert_eq!(hierarchy.application, "tilo.workspace");
            assert_eq!(hierarchy.operation, "profile.save");
            assert!(provider.service_pending.borrow().is_empty());
            finish_local_index(&provider, &ctx);
            assert!(
                !registry.indexes_settling(&args),
                "the actual completed workspace-service listing may settle"
            );
        }
    }

    #[test]
    fn unanswered_workspace_service_page_retries_before_listing_settles() {
        for outcome in ["failed", "stale", "abandoned", "timed_out"] {
            let chrome = chrome();
            let owner = chrome.borrow().active_local_user();
            chrome.borrow().tree_handle().write().desktop.workspaces[0].set_owner(owner);
            let provider = LiveAppActionsProvider::new_with_fixture_identity(broker_for(&chrome));
            let doc = AutoCommit::new();
            let chrome_state = chrome.borrow();
            let ctx = SearchContext {
                query_scope: Some(crate::shell::chrome_state::SearchScope::All),
                ..context("profile.save", &chrome_state, &doc)
            };
            provider.query_page(ctx, None, SearchBudget::PALETTE);
            finish_local_index(&provider, &ctx);
            assert!(provider.auxiliary_snapshot.borrow().is_some());
            let key = provider
                .service_pending
                .borrow()
                .keys()
                .next()
                .unwrap()
                .clone();
            let (sender, receiver) = pending();
            let now = Instant::now();
            provider.service_pending.borrow_mut().insert(
                key,
                PendingDirectoryPage {
                    receiver,
                    deadline: if outcome == "timed_out" {
                        now
                    } else {
                        now + Duration::from_secs(3)
                    },
                    initial_page: true,
                },
            );
            let unanswered = match outcome {
                "failed" => {
                    sender.complete(Err(tilo_actions::remote::ActionHostError::Unavailable {
                        reason: "source changed during listing".into(),
                    }));
                    None
                }
                "stale" => {
                    sender.complete(Ok(CataloguePage {
                        stale_cursor: true,
                        ..CataloguePage::default()
                    }));
                    None
                }
                "abandoned" => {
                    drop(sender);
                    None
                }
                "timed_out" => Some(sender),
                _ => unreachable!(),
            };
            let epoch = provider.directory_epoch.load(Ordering::Relaxed);
            provider.poll_service_pages_at(now);
            assert!(provider.service_pending.borrow().is_empty());
            assert!(provider.auxiliary_snapshot.borrow().is_none());
            assert!(provider.directory_epoch.load(Ordering::Relaxed) > epoch);
            let retried = provider.query_page(ctx, None, SearchBudget::PALETTE);
            assert!(
                retried
                    .results
                    .iter()
                    .all(|row| !row.id.starts_with("service\u{1f}"))
            );
            assert!(!provider.service_pending.borrow().is_empty());
            assert!(
                provider.index_settling(&ctx),
                "{outcome}: a fresh page is still owed"
            );
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(3))
                .expect("the fixture deadline fits the monotonic clock");
            loop {
                let completed = provider.query_page(ctx, None, SearchBudget::PALETTE);
                if completed
                    .results
                    .iter()
                    .any(|row| row.id.ends_with("\u{1f}profile.save"))
                {
                    break;
                }
                assert!(
                    provider.index_settling(&ctx),
                    "{outcome}: the unanswered real broker page cannot settle"
                );
                assert!(
                    Instant::now() < deadline,
                    "{outcome}: the actual broker profile action did not arrive"
                );
                std::thread::yield_now();
            }
            finish_local_index(&provider, &ctx);
            assert!(
                !provider.index_settling(&ctx),
                "{outcome}: actual broker rows settled"
            );
            drop(unanswered);
        }
    }

    #[test]
    fn auxiliary_service_query_releases_tree_before_a_queued_native_writer() {
        let (completed, observed) = std::sync::mpsc::sync_channel(1);
        let query = std::thread::spawn(move || {
            let chrome = chrome();
            let tree = chrome.borrow().tree_handle();
            let provider = LiveAppActionsProvider::new_with_fixture_identity(broker_for(&chrome));
            let reached = Rc::new(Cell::new(false));
            let reached_probe = Rc::clone(&reached);
            *provider.auxiliary_service_probe.borrow_mut() = Some(Box::new(move || {
                // Hold a test read only until the actual writer is queued.
                // try_read refusal proves parking_lot writer precedence;
                // elapsed time bounds a broken test, never proves queuing.
                let read = tree.read();
                let writer_tree = tree.clone();
                let (started, starting) = std::sync::mpsc::sync_channel(1);
                let (written, writing) = std::sync::mpsc::sync_channel(1);
                let writer = std::thread::spawn(move || {
                    started.send(()).unwrap();
                    let mut tree = writer_tree.write();
                    tree.desktop.external_revision += 1;
                    drop(tree);
                    written.send(()).unwrap();
                });
                starting.recv_timeout(Duration::from_secs(2)).unwrap();
                let began = Instant::now();
                while tree.try_read().is_some() {
                    assert!(
                        began.elapsed() < Duration::from_secs(2),
                        "native writer never queued"
                    );
                    std::thread::yield_now();
                }
                drop(read);
                // The old production if-let guard would still pin a read
                // here, preventing both this writer and the nested query.
                writing
                    .recv_timeout(Duration::from_secs(2))
                    .expect("provider retained a tree read across service callbacks");
                writer.join().unwrap();
                reached_probe.set(true);
            }));
            let doc = AutoCommit::new();
            let page = provider.query_page(
                context("", &chrome.borrow(), &doc),
                None,
                SearchBudget::PALETTE,
            );
            assert!(
                reached.get(),
                "actual auxiliary service query was not exercised"
            );
            assert!(!page.revision.is_empty());
            completed.send(()).unwrap();
        });
        observed
            .recv_timeout(Duration::from_secs(8))
            .expect("actual provider query did not complete after the native writer");
        query.join().unwrap();
    }

    #[test]
    fn room_epoch_change_retires_an_empty_workspace_service_page() {
        use collab_types::{CapOverride, Verb};

        fn profile_save_visible(
            provider: &LiveAppActionsProvider,
            chrome: &crate::shell::chrome_state::Chrome,
            doc: &AutoCommit,
        ) -> bool {
            provider
                .query_page(
                    context("profile.save", &chrome.borrow(), doc),
                    None,
                    SearchBudget::PALETTE,
                )
                .results
                .iter()
                .any(|row| row.id.ends_with("\u{1f}profile.save"))
        }

        let chrome = chrome();
        let local_user = chrome.borrow().active_local_user();
        {
            let handle = chrome.borrow().tree_handle();
            let mut tree = handle.write();
            let workspace = &mut tree.desktop.workspaces[0];
            workspace.set_owner("remote-owner");
            workspace.grant_capability(CapOverride::membership(
                collab_types::Subject::person(local_user.clone()),
                workspace.room.to_string(),
                Verb::Write,
            ));
        }
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker_for(&chrome));
        let doc = AutoCommit::new();

        assert!(!profile_save_visible(&provider, &chrome, &doc));
        assert!(!profile_save_visible(&provider, &chrome, &doc));
        assert!(
            !provider.service_pages.borrow().is_empty(),
            "the denied Editor response is retained before native projection"
        );

        {
            let handle = chrome.borrow().tree_handle();
            let mut tree = handle.write();
            tree.desktop.workspaces[0].set_owner(local_user);
            // This is the cache boundary advanced by
            // `WorkspaceTree::project_native_room`; the signed projection is
            // covered at that owner's integration boundary.
            tree.desktop.room_epoch = tree.desktop.room_epoch.wrapping_add(1);
        }

        assert!(!profile_save_visible(&provider, &chrome, &doc));
        assert!(
            profile_save_visible(&provider, &chrome, &doc),
            "the identical query must observe authority from the new native-room projection"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end broker service scenario keeps cold discovery, invocation, and revocation together"
    )]
    fn opaque_workspace_service_rows_use_the_broker_catalogue_and_invocation_door() {
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, ResourceRef};
        use crate::shell::search::visibility::CapabilityVisibility;
        use crate::shell::state::WorkspaceAuthority as _;

        let chrome = chrome();
        let owner = chrome.borrow().active_local_user();
        chrome.borrow().tree_handle().write().desktop.workspaces[0].set_owner(owner.clone());
        let broker = broker_for(&chrome);
        let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker));
        let doc = AutoCommit::new();
        let mut first_doc = AutoCommit::new();

        let first = provider.query_page(
            context("profile.save", &chrome.borrow(), &doc),
            None,
            SearchBudget::PALETTE,
        );
        assert!(
            first
                .results
                .iter()
                .all(|row| !row.id.starts_with("service\u{1f}")),
            "a workspace-service row is not fabricated before its catalogue replies"
        );
        let second = provider.query_page(
            context("profile.save", &chrome.borrow(), &doc),
            None,
            SearchBudget::PALETTE,
        );
        let row = second
            .results
            .into_iter()
            .find(|row| row.id.ends_with("\u{1f}profile.save"))
            .expect("the broker's workspace service row is published");
        let hierarchy = provider
            .action_hierarchy(&row.id)
            .expect("the exact retained row supplies its hierarchy");
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        assert_eq!(hierarchy.application, "tilo.workspace");
        assert_eq!(hierarchy.room, room);
        assert_eq!(hierarchy.operation, "profile.save");

        let resource = ResourceRef::new(PROVIDER_ID, row.id.clone());
        assert_eq!(
            crate::shell::search::visibility::required_capability(&resource, &ResourceKind::Action),
            Some((
                crate::shell::state::ResourceRef::Action {
                    app: hierarchy.application.clone(),
                    resource: hierarchy.resource.clone(),
                    action: hierarchy.operation,
                },
                crate::shell::state::Verb::Admin,
            )),
            "the service capability must retain the exact published resource"
        );
        let mut registry = SearchRegistry::new();
        registry.register(provider.clone());
        let chrome_state = chrome.borrow();
        let workspace = chrome_state.tree().desktop.workspaces[0].clone();
        let allowed = CapabilityVisibility::new(&workspace, workspace.actor_for_person(&owner));
        let catalogue = ActionCatalog::build(
            &registry,
            "profile.save",
            crate::shell::chrome_state::SearchScope::All,
            None,
            &chrome_state,
            &doc,
            &automerge::ROOT,
            &allowed,
        );
        assert!(
            catalogue.contains(&resource),
            "the native catalogue retains the broker-authenticated Admin capability"
        );
        let denied = CapabilityVisibility::new(
            &workspace,
            workspace.actor_for_person(&"not-an-administrator".to_string()),
        );
        let hidden = ActionCatalog::build(
            &registry,
            "profile.save",
            crate::shell::chrome_state::SearchScope::All,
            None,
            &chrome_state,
            &doc,
            &automerge::ROOT,
            &denied,
        );
        assert!(
            !hidden.contains(&resource),
            "the same opaque row stays absent for a caller without Admin"
        );
        drop(chrome_state);

        let (cached_room, cached_actor, cached_needle, cached_revision, mut malformed) = provider
            .service_pages
            .borrow()
            .iter()
            .find_map(|((cached_room, cached_actor, cached_needle), page)| {
                page.rows.iter().find_map(|candidate| {
                    candidate.action.as_ref().and_then(|descriptor| {
                        (descriptor.address.action == "profile.save").then(|| {
                            (
                                cached_room.clone(),
                                cached_actor.clone(),
                                cached_needle.clone(),
                                page.revision.clone(),
                                candidate.clone(),
                            )
                        })
                    })
                })
            })
            .expect("the exact broker row remains cached");
        malformed.app_id.push(ID_SEPARATOR);
        assert!(
            provider
                .service_action_result(
                    &cached_room,
                    &cached_revision,
                    &malformed,
                    &cached_needle,
                    &cached_actor,
                )
                .is_none(),
            "a separator in retained metadata cannot mint an ambiguous service id"
        );

        let mut supplied = serde_json::Map::new();
        supplied.insert("name".into(), "Fixture profile".into());
        supplied.insert("description".into(), "Broker-backed".into());
        supplied.insert(
            "rules".into(),
            serde_json::json!([{
                "scope": {"Action": {"app": "so.tilo.chat", "resource": "chat.person:tide", "action": "open-agent-conversation"}},
                "effect": "Grant",
                "verb": "Write"
            }]),
        );
        supplied.insert("confirm".into(), true.into());
        let args = crate::shell::search::action_api::ActionArgs::bind_json(&row.params, &supplied)
            .expect("arguments follow the published descriptor");
        let mut edits = ui::widget::Edits::lend(&mut first_doc);
        let disposition = row
            .on_begin_invoke
            .as_ref()
            .expect("workspace service admission is asynchronous")(
            &mut edits,
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .expect("the current owner is admitted");
        let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
            panic!("workspace service starts through the retained broker ticket");
        };
        let receipt = ticket()
            .expect("broker admission does not refuse")
            .expect("the eager native service answers immediately");
        assert!(receipt.broker_invocation_id.is_some());
        assert!(
            chrome.borrow().tree().desktop.workspaces[0]
                .permission_profiles
                .iter()
                .any(|profile| profile.name == "Fixture profile"),
            "the broker service, rather than the local search callback, applied the operation"
        );
        let _ = provider.query_page(
            context("profile.save", &chrome.borrow(), &doc),
            None,
            SearchBudget::PALETTE,
        );
        let fresh_row = provider
            .query_page(
                context("profile.save", &chrome.borrow(), &doc),
                None,
                SearchBudget::PALETTE,
            )
            .results
            .into_iter()
            .find(|candidate| candidate.id.ends_with("\u{1f}profile.save"))
            .expect("the changed workspace republishes a fresh governance row");

        let mut foreign = row.id.split(ID_SEPARATOR).collect::<Vec<_>>();
        foreign[1] = "workspace:00000000-0000-0000-0000-000000000001";
        assert!(
            provider
                .resolve(
                    &foreign.join(&ID_SEPARATOR.to_string()),
                    context("", &chrome.borrow(), &doc),
                )
                .is_none(),
            "an opaque provider for another room never inherits the cached action"
        );
        let mut forged_app = row.id.split(ID_SEPARATOR).collect::<Vec<_>>();
        forged_app[3] = "so.tilo.chat";
        assert!(
            provider
                .resolve(
                    &forged_app.join(&ID_SEPARATOR.to_string()),
                    context("", &chrome.borrow(), &doc),
                )
                .is_none(),
            "an app label in the opaque id cannot replace the retained descriptor"
        );
        let mut forged_permission = row.id.split(ID_SEPARATOR).collect::<Vec<_>>();
        forged_permission[4] = "none";
        assert!(
            provider
                .resolve(
                    &forged_permission.join(&ID_SEPARATOR.to_string()),
                    context("", &chrome.borrow(), &doc),
                )
                .is_none(),
            "a permission in the opaque id cannot replace the retained contract"
        );
        let mut stale_revision = row.id.split(ID_SEPARATOR).collect::<Vec<_>>();
        stale_revision[2] = "unobserved-revision";
        assert!(
            provider
                .resolve(
                    &stale_revision.join(&ID_SEPARATOR.to_string()),
                    context("", &chrome.borrow(), &doc),
                )
                .is_none(),
            "an unobserved catalogue revision cannot be invoked"
        );

        {
            let handle = chrome.borrow().tree_handle();
            let mut tree = handle.write();
            tree.desktop.workspaces[0].set_owner("another-person");
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        }
        let denied = fresh_row
            .on_begin_invoke
            .as_ref()
            .expect("the retained result keeps only the broker invocation door")(
            &mut edits,
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .expect("starting an admission ticket is not an authority claim");
        let crate::shell::search::InvocationDisposition::Pending(denied) = denied else {
            panic!("workspace service authority is decided by the retained broker ticket");
        };
        assert!(
            denied().is_err(),
            "the old result must fail the broker's fresh authority check"
        );
        let _ = provider.query_page(
            context("profile.save", &chrome.borrow(), &doc),
            None,
            SearchBudget::PALETTE,
        );
        let revoked = provider.query_page(
            context("profile.save", &chrome.borrow(), &doc),
            None,
            SearchBudget::PALETTE,
        );
        assert!(
            revoked
                .results
                .iter()
                .all(|candidate| !candidate.id.ends_with("\u{1f}profile.save")),
            "a caller removed from current workspace authority cannot retain a cached service row"
        );
        assert!(
            provider.action_hierarchy(&fresh_row.id).is_none(),
            "a refreshed context retires the old cached hierarchy"
        );
    }

    fn page(rows: Vec<CatalogueRow>, next_cursor: Vec<u8>) -> CataloguePage {
        CataloguePage {
            revision: "r1".into(),
            exhausted: next_cursor.is_empty(),
            budget_exhausted: !next_cursor.is_empty(),
            rows,
            next_cursor,
            ..CataloguePage::default()
        }
    }

    fn local_action(label: &str, action: &str) -> PublishedAction {
        PublishedAction {
            semantic_identity: None,
            resource: format!("resource:{action}"),
            action: action.into(),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            action_label: label.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: tilo_actions::ActionContract::view(),
            view: None,
            input: PublishedInput::None,
        }
    }

    #[test]
    fn cached_directory_action_replaces_only_its_exact_local_alias() {
        let chrome = chrome();
        let broker = broker_for(&chrome);
        let state = chrome.borrow();
        let cluster_id = state.tree().desktop.workspaces[0].room.to_string();
        let actor = state.local_user_for_room(&state.tree().desktop.workspaces[0].room);
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.fixture".into(),
            route: CrdtRoute::app("so.tilo.fixture", &cluster_id),
            actions: vec![
                local_action("Inspect playback", "inspect"),
                local_action("Inspect playback", "inspect-other"),
            ],
            live: true,
        };
        broker.replace_published_surfaces_for_test(vec![surface.clone()]);
        let root = ActionDirectoryRoot {
            peer_id: surface.peer_id,
            app_id: surface.app_id.clone(),
            cluster_id,
            record_id: surface.route.record_id.clone(),
        };
        broker.replace_action_directory_roots_for_test(vec![root]);
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
        let doc = AutoCommit::new();
        let ctx = context("Inspect playback", &state, &doc);
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let before = provider.query_page(ctx, None, SearchBudget::PALETTE);
        assert_eq!(
            before.results.len(),
            2,
            "cold directories retain the local action"
        );
        let row = CatalogueRow {
            provider: "7".into(),
            app_id: surface.app_id.clone(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "resource:inspect".into(),
                kind: "fixture".into(),
                label: "Fixture".into(),
                presentation: None,
            },
            action: Some(exported("Inspect playback", "inspect")),
            has_children: false,
            fresh_binding: None,
            visibility: None,
        };
        let key = (7, actor, "inspect playback".into(), Vec::new());
        provider
            .directory_pages
            .borrow_mut()
            .insert(key.clone(), page(vec![row], Vec::new()));
        provider
            .directory_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cached = provider.query_page(ctx, None, SearchBudget::PALETTE);
        assert_eq!(
            cached.results.len(),
            2,
            "replace the exact alias while preserving a different capability with the same label"
        );
        assert_eq!(
            cached
                .results
                .iter()
                .filter(|row| row.id.starts_with("directory\u{1f}"))
                .count(),
            1
        );
        assert!(cached.results.iter().any(|row| {
            row.id == LiveAppActionsProvider::result_id(&surface, &surface.actions[1])
        }));
        provider.directory_pages.borrow_mut().remove(&key);
        provider
            .directory_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let restored = provider.query_page(ctx, None, SearchBudget::PALETTE);
        assert_eq!(restored.results.len(), 2);
        assert!(restored.results.iter().any(|row| {
            row.id == LiveAppActionsProvider::result_id(&surface, &surface.actions[0])
        }));
    }

    #[test]
    fn completed_directory_page_survives_idle_poll() {
        let provider = LiveAppActionsProvider::new(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let key: DirectoryCacheKey = (7, "actor".into(), "read".into(), Vec::new());
        let (sender, receiver) = pending();
        let deadline = Instant::now();
        provider.directory_pending.borrow_mut().insert(
            key.clone(),
            PendingDirectoryPage {
                receiver,
                deadline,
                initial_page: true,
            },
        );
        sender.complete(Ok(page(Vec::new(), Vec::new())));

        provider.poll_directory_pages_at(deadline + Duration::from_secs(3));

        assert_eq!(provider.directory_pages.borrow()[&key].revision, "r1");
        assert!(provider.directory_pending.borrow().is_empty());
        assert_eq!(
            provider
                .directory_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn expired_directory_page_retries_empty_projection() {
        let provider = LiveAppActionsProvider::new(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let (sender, receiver) = pending();
        let deadline = Instant::now();
        provider.directory_pending.borrow_mut().insert(
            (7, "actor".into(), "read".into(), Vec::new()),
            PendingDirectoryPage {
                receiver,
                deadline,
                initial_page: true,
            },
        );
        *provider.auxiliary_snapshot.borrow_mut() = Some(AuxiliarySnapshot {
            revision: "r1".into(),
            query: "read".into(),
            scope: String::new(),
            resume: AuxiliaryResume {
                root_after: None,
                directory: None,
            },
            entries: Vec::new(),
            pending_directory: Vec::new(),
            next_resume: None,
        });

        provider.poll_directory_pages_at(deadline + Duration::from_secs(3));

        assert!(sender.is_abandoned(), "unanswered work still expires");
        assert!(provider.directory_pending.borrow().is_empty());
        assert!(provider.auxiliary_snapshot.borrow().is_none());
        assert_eq!(
            provider
                .directory_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn directory_cache_is_bounded_expiring_and_scoped_to_current_authority() {
        // Phase `directory_lifecycle_invalidates_only_lost_current_authority`.
        {
            for error in [
                ActionHostError::UnknownAction,
                ActionHostError::InvalidRequest {
                    reason: "stale revision".into(),
                },
                ActionHostError::Unavailable {
                    reason: "owner left".into(),
                },
            ] {
                assert!(invalidates_directory_cache(&error));
            }
            for error in [
                ActionHostError::NotPermitted,
                ActionHostError::ConfirmationRequired,
                ActionHostError::InvocationConflict,
            ] {
                assert!(!invalidates_directory_cache(&error));
            }
        }

        // Phase `directory_cache_bounds_rapid_queries_and_expires_stalled_page_and_start_tickets`.
        {
            let provider = LiveAppActionsProvider::new(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
            );
            let mut prior_sender: Option<
                tilo_actions::remote::Completer<Result<CataloguePage, ActionHostError>>,
            > = None;
            for index in 0..10_000_usize {
                let needle = format!("needle-{index}");
                provider.set_directory_query(&needle);
                if let Some(sender) = prior_sender.take() {
                    assert!(
                        sender.is_abandoned(),
                        "changing a query must drop the prior palette receiver"
                    );
                }
                let key: DirectoryCacheKey = (7, "actor-a".into(), needle, Vec::new());
                provider
                    .directory_pages
                    .borrow_mut()
                    .insert(key.clone(), page(Vec::new(), Vec::new()));
                let (sender, receiver) = pending();
                provider.directory_pending.borrow_mut().insert(
                    key,
                    PendingDirectoryPage {
                        receiver,
                        deadline: super::deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
                        initial_page: true,
                    },
                );
                prior_sender = Some(sender);
                assert!(provider.directory_pages.borrow().len() <= 1);
                assert!(provider.directory_pending.borrow().len() <= 1);
            }
            provider.set_directory_query("new-needle");
            assert!(prior_sender.expect("final sender").is_abandoned());
            assert!(provider.directory_pages.borrow().is_empty());
            assert!(provider.directory_pending.borrow().is_empty());

            let expired = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            let (page_sender, page_receiver) = pending();
            provider.directory_pending.borrow_mut().insert(
                (7, "actor-a".into(), "new-needle".into(), Vec::new()),
                PendingDirectoryPage {
                    receiver: page_receiver,
                    deadline: expired,
                    initial_page: true,
                },
            );
            provider.poll_directory_pages_at(Instant::now());
            assert!(
                page_sender.is_abandoned(),
                "expired directory page ticket is cancelled"
            );
            assert!(provider.directory_pending.borrow().is_empty());

            let invalidations: Rc<RefCell<std::collections::HashSet<DirectoryCacheKey>>> =
                Rc::new(RefCell::new(std::collections::HashSet::default()));
            let (start_sender, start_receiver) = pending();
            let key = (7, "actor-a".into(), "new-needle".into(), Vec::new());
            let ticket = directory_start_ticket(
                start_receiver,
                Rc::clone(&invalidations),
                key.clone(),
                expired,
            );
            assert!(
                ticket().is_err(),
                "expired action admission is refused rather than retained"
            );
            drop(ticket);
            assert!(
                start_sender.is_abandoned(),
                "an expired action ticket lets go of its admission"
            );
            assert!(invalidations.borrow().contains(&key));
        }

        // Phase `directory_cache_keeps_room_actors_separate_for_the_current_needle`.
        {
            let provider = LiveAppActionsProvider::new(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
            );
            provider.set_directory_query("open");
            for actor in ["room-a-user", "room-b-user"] {
                provider.directory_pages.borrow_mut().insert(
                    (7, actor.into(), "open".into(), Vec::new()),
                    page(Vec::new(), Vec::new()),
                );
            }
            provider.set_directory_query("open");
            assert_eq!(provider.directory_pages.borrow().len(), 2);
            provider.set_directory_query("other");
            assert!(provider.directory_pages.borrow().is_empty());
        }
    }

    #[test]
    fn certified_job_status_keeps_valid_receipts_and_public_refusals_opaque() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::action_api::{
            ActionCatalog, ActionError, AllVisible, ResourceRef,
        };
        use crate::shell::search::{SearchRegistry, SearchResult};

        struct OneResult(SearchResult);

        impl SearchProvider for OneResult {
            fn id(&self) -> &'static str {
                PROVIDER_ID
            }

            fn name(&self) -> &'static str {
                "certified job fixture"
            }

            fn resource_kinds(&self) -> &[ResourceKind] {
                std::slice::from_ref(&self.0.kind)
            }

            fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
                vec![self.0.clone()]
            }
        }

        fn invoke(
            row: SearchResult,
            chrome: &crate::shell::chrome_state::Chrome,
            doc: &mut AutoCommit,
        ) -> Result<crate::shell::search::InvocationDisposition, ActionError> {
            let resource = ResourceRef::new(&row.provider_id, &row.id);
            let mut registry = SearchRegistry::new();
            registry.register(Rc::new(OneResult(row)));
            let catalog = ActionCatalog::build_for_resource(
                &registry,
                &resource,
                SearchScope::All,
                None,
                &chrome.borrow(),
                doc,
                &automerge::ROOT,
                &AllVisible,
            );
            catalog
                .select(&resource)
                .expect("the fixture result is selected by exact address")
                .invoke_json_disposition(
                    &mut ui::widget::Edits::lend(doc),
                    &automerge::ROOT,
                    chrome,
                    &serde_json::Map::new(),
                )
        }

        let chrome = chrome();
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
        let actor = chrome.borrow().active_local_user();
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        broker.record_completion_for_test(17, &actor, succeeded(17));
        let mut doc = AutoCommit::new();

        let valid = provider.certified_job_result(17, "succeeded", false, &room, &actor);
        let disposition =
            invoke(valid, &chrome, &mut doc).expect("the exact fixture caller retains its job");
        let crate::shell::search::InvocationDisposition::Accepted(receipt) = disposition else {
            panic!("an existing certified receipt resolves immediately");
        };
        assert_eq!(receipt.broker_invocation_id, Some(17));
        assert_eq!(receipt.update, succeeded(17));

        let missing = provider.certified_job_result(99, "accepted", false, &room, &actor);
        let Err(error) = invoke(missing, &chrome, &mut doc) else {
            panic!("a foreign or absent job must stay unavailable");
        };
        assert_eq!(error.code(), "action_failed");
        assert_eq!(error.to_string(), super::JOB_UNAVAILABLE_MESSAGE);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_action_pages_never_repeat_and_cursors_are_self_contained() {
        // Phase `remote_actions_page_without_repeats_under_tiny_budgets`.
        {
            let provider = LiveAppActionsProvider::new_with_fixture_identity(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
            );
            let chrome = chrome();
            let chrome = chrome.borrow();
            let doc = AutoCommit::new();
            provider.set_directory_query("open");
            let ctx = context("open", &chrome, &doc);
            let revision = provider.search_revision(&ctx);
            let scope = LiveAppActionsProvider::surface_scope(&ctx);
            let root = ActionDirectoryRoot {
                peer_id: 7,
                app_id: "so.tilo.fixture".into(),
                cluster_id: "room".into(),
                record_id: "record".into(),
            };
            assert_eq!(
                provider
                    .directory_action_result(
                        &root,
                        "r1",
                        &exported("Open one", "one"),
                        "open",
                        "fixture",
                        &[]
                    )
                    .id,
                format!(
                    "directory{ID_SEPARATOR}7{ID_SEPARATOR}r1{ID_SEPARATOR}resource:one{ID_SEPARATOR}one"
                )
            );
            *provider.auxiliary_snapshot.borrow_mut() = Some(AuxiliarySnapshot {
                revision,
                query: "open".into(),
                scope,
                resume: AuxiliaryResume {
                    root_after: None,
                    directory: None,
                },
                entries: ["one", "two", "three"]
                    .into_iter()
                    .map(|suffix| AuxiliaryEntry::Directory {
                        root: root.clone(),
                        revision: "r1".into(),
                        descriptor: exported(&format!("Open {suffix}"), suffix),
                        actor: "fixture".into(),
                        cursor: Vec::new(),
                    })
                    .collect(),
                pending_directory: Vec::new(),
                next_resume: None,
            });
            let budget = SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            };
            let mut cursor = None;
            let mut ids = Vec::new();
            for _ in 0..3 {
                let page =
                    provider.query_page(context("open", &chrome, &doc), cursor.as_ref(), budget);
                ids.extend(page.results.into_iter().map(|row| row.id));
                cursor = page.next_cursor;
            }
            assert_eq!(ids.len(), 3);
            let distinct = ids.iter().collect::<std::collections::HashSet<_>>();
            assert_eq!(
                distinct.len(),
                3,
                "a one-row budget must not repeat remote rows"
            );
            let last = provider.query_page(context("open", &chrome, &doc), cursor.as_ref(), budget);
            assert!(last.exhausted);

            // An action too large for this page is consumed once, not retried forever.
            let tiny = provider.query_page(
                context("open", &chrome, &doc),
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: 1,
                    max_work: 1,
                },
            );
            assert!(tiny.results.is_empty());
            assert!(tiny.next_cursor.is_some());
        }

        // Phase `cached_remote_pages_append_after_their_opaque_continuation`.
        {
            let chrome = chrome();
            let broker = broker_for(&chrome);
            let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
            let chrome = chrome.borrow();
            let cluster_id = chrome.tree().desktop.workspaces[0].room.to_string();
            let actor = chrome.local_user_for_room(&chrome.tree().desktop.workspaces[0].room);
            let root = ActionDirectoryRoot {
                peer_id: 9,
                app_id: "so.tilo.remote".into(),
                cluster_id,
                record_id: "root".into(),
            };
            broker.replace_action_directory_roots_for_test(vec![root]);
            let row = |label: &str, action: &str| CatalogueRow {
                provider: "9".into(),
                app_id: "so.tilo.remote".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: format!("resource:{action}"),
                    kind: "fixture".into(),
                    label: "Fixture".into(),
                    presentation: None,
                },
                action: Some(exported(label, action)),
                has_children: false,
                fresh_binding: None,
                visibility: None,
            };
            provider.directory_pages.borrow_mut().insert(
                (9, actor.clone(), "open".into(), Vec::new()),
                page(vec![row("Open first", "first")], vec![1]),
            );
            provider.directory_pages.borrow_mut().insert(
                (9, actor, "open".into(), vec![1]),
                page(vec![row("Open second", "second")], Vec::new()),
            );
            let doc = AutoCommit::new();
            provider.set_directory_query("open");
            let budget = SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            };
            let first = provider.query_page(context("open", &chrome, &doc), None, budget);
            let second = provider.query_page(
                context("open", &chrome, &doc),
                first.next_cursor.as_ref(),
                budget,
            );
            assert_eq!(first.results[0].title, "Open first");
            assert_eq!(second.results[0].title, "Open second");
            assert_ne!(first.results[0].id, second.results[0].id);
        }

        // Phase `cold_public_action_page_does_not_start_a_surface_clone_or_index_build`.
        {
            let provider = LiveAppActionsProvider::new(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
            );
            let chrome = chrome();
            let chrome = chrome.borrow();
            let doc = AutoCommit::new();
            let page = provider.query_page(
                context("needle", &chrome, &doc),
                None,
                SearchBudget::PALETTE,
            );
            assert!(page.results.is_empty());
            assert!(
                provider.surface_build.borrow().is_none(),
                "the query path may only observe a ready retained surface index"
            );
        }

        // Phase `auxiliary_cursor_is_self_contained_for_more_than_128_pages`.
        {
            let root = ActionDirectoryRoot {
                peer_id: 42,
                app_id: "so.tilo.remote".into(),
                cluster_id: "room\u{1f}with-separator".into(),
                record_id: "record".into(),
            };
            let resume = AuxiliaryResume {
                root_after: Some(41),
                directory: Some(super::DirectoryContinuation {
                    root,
                    actor: "person:alice".into(),
                    cursor: vec![0, 255, 1, 2],
                    row_offset: 100_001,
                }),
            };
            let encoded = LiveAppActionsProvider::encode_auxiliary_resume(&resume);
            assert_eq!(
                LiveAppActionsProvider::decode_auxiliary_resume(&encoded),
                Some(resume),
                "a valid continuation cannot depend on a bounded in-memory cursor table"
            );
        }

        // Phase `malformed_auxiliary_cursor_fails_closed`.
        {
            assert!(LiveAppActionsProvider::decode_auxiliary_resume("not;a,cursor").is_none());
        }
    }

    #[test]
    fn programmatic_directory_actions_resolve_without_entering_palette_search() {
        let chrome = chrome();
        let broker = broker_for(&chrome);
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
        let state = chrome.borrow();
        let workspace_room = state.tree().desktop.workspaces[0].room;
        let actor = state.local_user_for_room(&workspace_room);
        let root = ActionDirectoryRoot {
            peer_id: 9,
            app_id: "so.tilo.browser".into(),
            cluster_id: workspace_room.to_string(),
            record_id: "root".into(),
        };
        broker.replace_action_directory_roots_for_test(vec![root]);
        let mut descriptor = exported("Read page content", "read");
        descriptor
            .contract
            .tags
            .push(tilo_actions::PROGRAMMATIC_TAG.into());
        let row = CatalogueRow {
            provider: "9".into(),
            app_id: "so.tilo.browser".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: descriptor.address.resource.clone(),
                kind: "fixture".into(),
                label: "Fixture".into(),
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            fresh_binding: None,
            visibility: None,
        };
        provider.set_directory_query("read");
        provider.directory_pages.borrow_mut().insert(
            (9, actor, "read".into(), Vec::new()),
            page(vec![row], Vec::new()),
        );
        let doc = AutoCommit::new();
        let id = format!(
            "directory{ID_SEPARATOR}9{ID_SEPARATOR}r1{ID_SEPARATOR}resource:read{ID_SEPARATOR}read"
        );
        assert!(provider.query(context("read", &state, &doc)).is_empty());
        assert!(
            provider
                .query_page(context("read", &state, &doc), None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        let discovered = provider.query_page(
            SearchContext {
                include_programmatic: true,
                ..context("read", &state, &doc)
            },
            None,
            SearchBudget::PALETTE,
        );
        assert_eq!(discovered.results.len(), 1);
        assert_eq!(discovered.results[0].id, id);
        assert!(
            discovered.results[0]
                .contract
                .as_ref()
                .is_some_and(tilo_actions::ActionContract::is_programmatic)
        );
        let resolved = provider.resolve(&id, context("", &state, &doc));
        assert!(
            resolved.is_some(),
            "a cached programmatic directory action must resolve by address"
        );
        assert!(
            provider
                .resolve(&id.replace("r1", "stale"), context("", &state, &doc))
                .is_none()
        );
    }

    fn assert_machine_catalogue_visibility(
        registry: &crate::shell::search::SearchRegistry,
        resource: &crate::shell::search::action_api::ResourceRef,
        chrome: &crate::shell::chrome_state::Chrome,
        doc: &AutoCommit,
    ) {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        let state = chrome.borrow();
        let args = crate::shell::search::QueryArgs {
            needle: "Read page content",
            scope: crate::shell::chrome_state::SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc,
            doc_root: &automerge::ROOT,
        };
        let normal = ActionCatalog::build_query(registry, &args, &AllVisible, false);
        assert!(!normal.contains(resource));
        let started = Instant::now();
        let machine = loop {
            let catalogue = ActionCatalog::build_query(registry, &args, &AllVisible, true);
            if catalogue.contains(resource) || started.elapsed() >= Duration::from_secs(5) {
                break catalogue;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(machine.contains(resource));
        assert!(
            machine.enumerate()[0]
                .contract
                .as_ref()
                .is_some_and(tilo_actions::ActionContract::is_programmatic)
        );
        let refused = ActionCatalog::build_query(
            registry,
            &args,
            &|candidate: &ResourceRef, _: &ResourceKind| candidate != resource,
            true,
        );
        assert!(
            !refused.contains(resource),
            "machine discovery keeps the visibility gate"
        );
    }

    #[test]
    fn programmatic_actions_resolve_by_address_without_entering_palette_search() {
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let mut programmatic = local_action("Read page content", "read");
        programmatic
            .contract
            .tags
            .push(tilo_actions::PROGRAMMATIC_TAG.into());
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.fixture".into(),
            route: CrdtRoute::app("so.tilo.fixture", &room),
            actions: vec![local_action("Open address", "navigate"), programmatic],
            live: true,
        };
        let id = LiveAppActionsProvider::result_id(&surface, &surface.actions[1]);
        broker.replace_published_surfaces_for_test(vec![surface]);
        let provider = Rc::new(LiveAppActionsProvider::new(broker.clone()));
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let ctx = context("", &state, &doc);
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let page = provider.query_page(ctx, None, SearchBudget::PALETTE);
        assert_eq!(page.results.len(), 1);
        assert!(page.results.iter().all(|row| row.id != id));
        assert!(
            provider
                .query_page(
                    context("Read page content", &state, &doc),
                    None,
                    SearchBudget::PALETTE
                )
                .results
                .is_empty()
        );
        drop(state);
        let mut registry = SearchRegistry::new();
        registry.register(provider);
        let resource = ResourceRef::new(PROVIDER_ID, id);
        assert_machine_catalogue_visibility(&registry, &resource, &chrome, &doc);
        let catalog = ActionCatalog::build_for_resource(
            &registry,
            &resource,
            crate::shell::chrome_state::SearchScope::Workspace,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(
            catalog.select(&resource).is_some(),
            "the published programmatic action must be addressable"
        );
    }

    #[test]
    fn exact_local_action_resolves_beyond_the_preview_and_rejects_stale_or_other_scopes() {
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.fixture".into(),
            route: CrdtRoute::app("so.tilo.fixture", &room),
            actions: (0..400)
                .map(|index| local_action(&format!("action {index}"), &format!("action-{index}")))
                .collect(),
            live: true,
        };
        let id = LiveAppActionsProvider::result_id(&surface, &surface.actions[399]);
        broker.replace_published_surfaces_for_test(vec![surface]);
        let provider = Rc::new(LiveAppActionsProvider::new(broker.clone()));
        let doc = AutoCommit::new();
        let chrome_state = chrome.borrow();
        let ctx = context("", &chrome_state, &doc);
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let preview = provider.query_page(ctx, None, SearchBudget::PALETTE);
        drop(chrome_state);
        assert!(preview.results.iter().all(|row| row.id != id));
        let mut registry = SearchRegistry::new();
        registry.register(provider.clone());
        let resource = ResourceRef::new(PROVIDER_ID, id.clone());
        let catalog = ActionCatalog::build_for_resource(
            &registry,
            &resource,
            crate::shell::chrome_state::SearchScope::Workspace,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(catalog.select(&resource).is_some());
        chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::ActiveApp;
        assert!(
            provider
                .resolve(&id, context("", &chrome.borrow(), &doc))
                .is_none()
        );
        chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::Workspace;
        broker.replace_published_surfaces_for_test(Vec::new());
        assert!(
            provider
                .resolve(&id, context("", &chrome.borrow(), &doc))
                .is_none()
        );
    }

    fn exact_catalogue_contains(
        registry: &crate::shell::search::SearchRegistry,
        resource: &crate::shell::search::action_api::ResourceRef,
        chrome: &crate::shell::chrome_state::Chrome,
        doc: &AutoCommit,
    ) -> bool {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible};
        ActionCatalog::build_for_resource(
            registry,
            resource,
            crate::shell::chrome_state::SearchScope::All,
            None,
            &chrome.borrow(),
            doc,
            &automerge::ROOT,
            &AllVisible,
        )
        .contains(resource)
    }

    #[test]
    fn canonical_all_scope_does_not_inherit_the_active_app_palette_filter() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.notes".into(),
            route: CrdtRoute::app("so.tilo.notes", &room),
            actions: vec![local_action("Read notebook", "read")],
            live: true,
        };
        let resource = ResourceRef::new(
            PROVIDER_ID,
            LiveAppActionsProvider::result_id(&surface, &surface.actions[0]),
        );
        broker.replace_published_surfaces_for_test(vec![surface]);
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(LiveAppActionsProvider::new(broker)));
        chrome.borrow_mut().search_scope = SearchScope::ActiveApp;
        let doc = AutoCommit::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let all = loop {
            let all = ActionCatalog::build(
                &registry,
                "so.tilo.notes",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &automerge::ROOT,
                &AllVisible,
            );
            if all.contains(&resource) || Instant::now() >= deadline {
                break all;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(
            all.contains(&resource),
            "canonical All must include an inactive app"
        );
        assert!(exact_catalogue_contains(
            &registry, &resource, &chrome, &doc
        ));
        let palette = ActionCatalog::build(
            &registry,
            "so.tilo.notes",
            SearchScope::ActiveApp,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(
            !palette.contains(&resource),
            "the palette keeps its active-app scope"
        );
    }

    #[test]
    fn disclosed_local_address_survives_unrelated_publication_but_not_changed_authority() {
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.notes".into(),
            route: CrdtRoute::app("so.tilo.notes", &room),
            actions: vec![local_action("Read notebook", "read")],
            live: true,
        };
        let resource = ResourceRef::new(
            PROVIDER_ID,
            LiveAppActionsProvider::result_id(&surface, &surface.actions[0]),
        );
        broker.replace_published_surfaces_for_test(vec![surface.clone()]);
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(LiveAppActionsProvider::new(broker.clone())));
        let doc = AutoCommit::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let catalogue = ActionCatalog::build(
                &registry,
                "Read notebook",
                crate::shell::chrome_state::SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &automerge::ROOT,
                &AllVisible,
            );
            if catalogue.contains(&resource) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ordinary catalogue must disclose the read"
            );
            std::thread::yield_now();
        }
        let unrelated = PublishedSurface {
            peer_id: 8,
            app_id: "so.tilo.settings".into(),
            route: CrdtRoute::app("so.tilo.settings", &room),
            actions: vec![local_action("Read settings", "read")],
            live: true,
        };
        broker.replace_published_surfaces_for_test(vec![surface.clone(), unrelated]);
        assert!(
            exact_catalogue_contains(&registry, &resource, &chrome, &doc),
            "another app's publication must not invalidate an unchanged disclosed address"
        );
        let mut changed = surface.clone();
        changed.actions[0].contract.permission = tilo_actions::PermissionRequirement::Admin;
        broker.replace_published_surfaces_for_test(vec![changed]);
        assert!(!exact_catalogue_contains(
            &registry, &resource, &chrome, &doc
        ));
        let mut removed = surface;
        removed.actions.clear();
        broker.replace_published_surfaces_for_test(vec![removed]);
        assert!(!exact_catalogue_contains(
            &registry, &resource, &chrome, &doc
        ));
    }

    #[test]
    fn retained_local_rows_reject_closed_windows_and_changed_authority() {
        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.store".into(),
            route: CrdtRoute::app("so.tilo.store", &room),
            actions: vec![local_action("Inspect catalogue", "inspect-catalogue")],
            live: true,
        };
        broker.replace_published_surfaces_for_test(vec![surface.clone()]);
        let provider = LiveAppActionsProvider::new(broker.clone());
        let doc = AutoCommit::new();
        let chrome = chrome.borrow();
        let ctx = context("", &chrome, &doc);
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let id = LiveAppActionsProvider::result_id(&surface, &surface.actions[0]);
        assert_eq!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .len(),
            1
        );

        let mut changed = surface.clone();
        changed.actions[0].contract.permission = tilo_actions::PermissionRequirement::Admin;
        broker.replace_published_surfaces_for_test(vec![changed]);
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty(),
            "a retained index must not advertise an address with obsolete authority"
        );
        assert!(provider.resolve(&id, ctx).is_none());

        broker.replace_published_surfaces_for_test(Vec::new());
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty(),
            "a closed window must disappear before the replacement index is ready"
        );
        assert!(provider.resolve(&id, ctx).is_none());

        let mut reopened = surface;
        reopened.peer_id = 8;
        reopened.actions[0].resource = "new-window:catalogue".into();
        broker.replace_published_surfaces_for_test(vec![reopened.clone()]);
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let fresh = provider.query_page(ctx, None, SearchBudget::PALETTE);
        assert_eq!(fresh.results.len(), 1);
        assert_eq!(
            fresh.results[0].id,
            LiveAppActionsProvider::result_id(&reopened, &reopened.actions[0])
        );
        assert!(provider.resolve(&fresh.results[0].id, ctx).is_some());
    }

    #[test]
    fn retained_local_rows_apply_current_programmatic_visibility() {
        let chrome = chrome();
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.store".into(),
            route: CrdtRoute::app("so.tilo.store", &room),
            actions: vec![local_action("Inspect catalogue", "inspect-catalogue")],
            live: true,
        };
        broker.replace_published_surfaces_for_test(vec![surface.clone()]);
        let provider = LiveAppActionsProvider::new(broker.clone());
        let doc = AutoCommit::new();
        let chrome = chrome.borrow();
        let ctx = context("", &chrome, &doc);
        *provider.surface_snapshot.borrow_mut() =
            Some(LiveAppActionsProvider::build_local_snapshot(
                &broker,
                &LiveAppActionsProvider::build_scope(&ctx),
                &provider.build_probe,
            ));
        let id = LiveAppActionsProvider::result_id(&surface, &surface.actions[0]);
        assert_eq!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .len(),
            1
        );

        let mut programmatic = surface;
        programmatic.actions[0]
            .contract
            .tags
            .push(tilo_actions::PROGRAMMATIC_TAG.into());
        broker.replace_published_surfaces_for_test(vec![programmatic]);
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty(),
            "current programmatic visibility supersedes the retained palette index"
        );
        assert!(
            provider.resolve(&id, ctx).is_some(),
            "programmatic actions remain exactly addressable"
        );
        assert_eq!(
            provider
                .query_page(
                    SearchContext {
                        include_programmatic: true,
                        ..ctx
                    },
                    None,
                    SearchBudget::PALETTE
                )
                .results
                .len(),
            1
        );
    }

    #[test]
    fn retained_local_rows_materialize_and_invoke_the_current_publication() {
        use crate::shell::search::action_api::ActionArgs;
        let chrome = chrome();
        chrome.borrow_mut().act_as("store-row-owner".into());
        chrome.borrow().tree_mut().desktop.workspaces[0].set_owner("store-row-owner");
        let broker = broker_for(&chrome);
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let surface = PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.store".into(),
            route: CrdtRoute::app("so.tilo.store", &room),
            actions: vec![local_action("Old catalogue", "inspect-catalogue")],
            live: true,
        };
        broker.replace_published_surfaces_for_test(vec![surface.clone()]);
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
        let mut doc = AutoCommit::new();
        {
            let chrome = chrome.borrow();
            let ctx = context("", &chrome, &doc);
            *provider.surface_snapshot.borrow_mut() =
                Some(LiveAppActionsProvider::build_local_snapshot(
                    &broker,
                    &LiveAppActionsProvider::build_scope(&ctx),
                    &provider.build_probe,
                ));
        }
        let mut old_peer = broker.capture_invocations_for_test(7);
        let mut new_peer = broker.capture_invocations_for_test(8);
        let mut current = surface;
        current.peer_id = 8;
        current.actions[0].action_label = "Current catalogue".into();
        broker.replace_published_surfaces_for_test(vec![current]);
        let row = provider
            .query_page(
                context("", &chrome.borrow(), &doc),
                None,
                SearchBudget::PALETTE,
            )
            .results
            .pop()
            .expect("unchanged address remains available");
        assert_eq!(row.title, "Current catalogue");
        let resolved = provider
            .resolve(&row.id, context("", &chrome.borrow(), &doc))
            .unwrap();
        assert_eq!(resolved.title, row.title);
        (row.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::bind(&[], []).unwrap(),
        )
        .unwrap();
        let delivered = new_peer
            .try_recv()
            .expect("browse callback routes to the current peer");
        assert_eq!(delivered.action, "inspect-catalogue");
        assert!(
            old_peer.try_recv().is_err(),
            "the replaced publisher receives no invocation"
        );
    }

    #[test]
    fn a_100001_action_snapshot_reaches_a_unique_tail_without_scanning_the_corpus() {
        let provider = LiveAppActionsProvider::new_with_fixture_identity(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let chrome = chrome();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = context("tailtoken", &chrome, &doc);
        let surface = PublishedSurface {
            peer_id: 1,
            app_id: "so.tilo.fixture".into(),
            route: CrdtRoute::app(
                "so.tilo.fixture",
                chrome.tree().desktop.workspaces[0].room.to_string(),
            ),
            actions: Vec::new(),
            live: true,
        };
        let entries = (0..=4_000)
            .map(|index| {
                let label = if index == 4_000 {
                    "tailtoken".to_string()
                } else {
                    format!("common action {index}")
                };
                (
                    surface.clone(),
                    local_action(&label, &format!("action-{index}")),
                )
            })
            .collect::<Vec<_>>();
        let mut published = surface;
        published.actions = entries.iter().map(|(_, action)| action.clone()).collect();
        provider
            .broker
            .replace_published_surfaces_for_test(vec![published]);
        let index = Some(index_actions(&entries, false));
        let machine_index = Some(index_actions(&entries, true));
        *provider.surface_snapshot.borrow_mut() = Some(SurfaceSnapshot {
            source_generation: provider.broker.surfaces_generation(),
            scope: LiveAppActionsProvider::surface_scope(&ctx),
            entries,
            index,
            machine_index,
        });
        let page = provider.query_page(
            ctx,
            None,
            SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            },
        );
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "tailtoken");
        assert!(
            page.exhausted,
            "the rare indexed tail candidate takes one inspected record"
        );
    }

    #[test]
    fn source_backed_cold_query_does_no_action_index_work_before_refresh() {
        let chrome = chrome();
        let room = chrome.borrow().tree().desktop.workspaces[0]
            .room
            .to_string();
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        broker.replace_published_surfaces_for_test(vec![PublishedSurface {
            peer_id: 1,
            app_id: "so.tilo.fixture".into(),
            route: CrdtRoute::app("so.tilo.fixture", room),
            actions: (0..=4_000)
                .map(|index| {
                    local_action(
                        if index == 4_000 {
                            "unique-source-tail"
                        } else {
                            "common source action"
                        },
                        &format!("action-{index}"),
                    )
                })
                .collect(),
            live: true,
        }]);
        let provider = LiveAppActionsProvider::new_with_fixture_identity(broker);
        chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::All;
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = context("unique-source-tail", &chrome, &doc);
        let query_thread = std::thread::current().id();
        assert!(provider.search_revision(&ctx).starts_with("indexing:"));
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        assert_eq!(
            provider
                .build_probe
                .builds
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(provider.surface_build.borrow().is_none());
        // Building the broker projection is intentionally real work.  Give a
        // slow shared CI worker a minute to finish rather than turning this
        // cold-path assertion into a scheduler-speed test.
        for _ in 0..12_000 {
            provider.refresh_search_index(&ctx);
            if provider.search_revision(&ctx).starts_with("ready:") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(provider.search_revision(&ctx).starts_with("ready:"));
        assert!(
            provider
                .surface_snapshot
                .borrow()
                .as_ref()
                .expect("ready snapshot")
                .entries
                .iter()
                .all(|(surface, _)| surface.actions.is_empty()),
            "indexed rows retain only surface metadata, never the aggregate action list"
        );
        assert_eq!(
            provider
                .build_probe
                .builds
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_ne!(
            *provider
                .build_probe
                .thread
                .lock()
                .expect("live action build thread lock"),
            Some(query_thread)
        );
        let page = provider.query_page(
            context("unique-source-tail", &chrome, &doc),
            None,
            SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            },
        );
        assert_eq!(page.results[0].title, "unique-source-tail");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn action_hierarchy_decodes_only_provider_minted_live_addresses() {
        // Phase `hierarchy_decodes_the_address_shape_minted_by_the_provider`.
        {
            let id = [
                "so.tilo.files",
                "write",
                "room-7",
                "file:/Brief.md",
                "files.entry:/Brief.md",
                "rename",
            ]
            .join(&ID_SEPARATOR.to_string());

            assert_eq!(
                LiveAppActionsProvider::hierarchy(&id),
                Some((
                    "so.tilo.files",
                    "room-7",
                    "file:/Brief.md",
                    "files.entry:/Brief.md",
                    "rename",
                ))
            );
            assert!(LiveAppActionsProvider::hierarchy("display.id#rename").is_none());
        }

        // Phase `action_hierarchy_resolves_exact_live_peers_including_zero_and_deep_roots`.
        {
            let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
            let provider = LiveAppActionsProvider::new(broker.clone());
            let mut roots = (0..=512)
                .map(|peer_id| ActionDirectoryRoot {
                    peer_id,
                    app_id: "dev.tilo.first".into(),
                    cluster_id: "first-room".into(),
                    record_id: "first-record".into(),
                })
                .collect::<Vec<_>>();
            roots.push(ActionDirectoryRoot {
                peer_id: 4096,
                app_id: "dev.tilo.calculator".into(),
                cluster_id: "calculator-room".into(),
                record_id: "calculator-record".into(),
            });
            broker.replace_action_directory_roots_for_test(roots);
            assert!(
                broker
                    .state()
                    .action_directory_roots()
                    .iter()
                    .all(|root| root.peer_id != 4096),
                "fixture must put the target beyond the first bounded root page"
            );
            let first = provider
                .action_hierarchy("directory\u{1f}0\u{1f}r1\u{1f}resource:first\u{1f}open")
                .unwrap();
            assert_eq!(first.application, "dev.tilo.first");
            assert_eq!(first.room, "first-room");
            assert_eq!(first.record, "first-record");
            let deep_id = "directory\u{1f}4096\u{1f}r1\u{1f}calculator.graph:curve2d:17\u{1f}edit";
            let deep = provider.action_hierarchy(deep_id).unwrap();
            assert_eq!(deep.application, "dev.tilo.calculator");
            assert_eq!(deep.room, "calculator-room");
            assert_eq!(deep.record, "calculator-record");
            assert_eq!(deep.resource, "calculator.graph:curve2d:17");
            assert_eq!(deep.operation, "edit");
            assert!(
                provider
                    .action_hierarchy("directory\u{1f}4095\u{1f}r1\u{1f}resource:missing\u{1f}open")
                    .is_none(),
                "an absent peer cannot borrow its next neighbor's identity"
            );
            broker.replace_action_directory_roots_for_test(Vec::new());
            assert!(
                provider.action_hierarchy(deep_id).is_none(),
                "a stale result cannot invent a disconnected peer's identity"
            );
        }

        // Phase `action_hierarchy_keeps_local_actions_but_rejects_non_actions_and_malformed_directory_ids`.
        {
            let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
            broker.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
                peer_id: 0,
                app_id: "dev.tilo.calculator".into(),
                cluster_id: "room".into(),
                record_id: "record".into(),
            }]);
            let provider = LiveAppActionsProvider::new(broker);
            let local = provider.action_hierarchy("dev.tilo.files\u{1f}write\u{1f}room-7\u{1f}file:/Brief.md\u{1f}files.entry:/Brief.md\u{1f}rename").unwrap();
            assert_eq!(local.application, "dev.tilo.files");
            assert_eq!(local.room, "room-7");
            assert_eq!(local.record, "file:/Brief.md");
            assert_eq!(local.resource, "files.entry:/Brief.md");
            assert_eq!(local.operation, "rename");
            for id in [
                "directory\u{1f}0\u{1f}r1\u{1f}resource:first\u{1f}open\u{1f}extra".to_string(),
                "directory\u{1f}not-a-peer\u{1f}r1\u{1f}resource:first\u{1f}open".into(),
                "directory\u{1f}0\u{1f}\u{1f}resource:first\u{1f}open".into(),
                "directory\u{1f}0\u{1f}r1\u{1f}\u{1f}open".into(),
                "directory\u{1f}0\u{1f}r1\u{1f}resource:first\u{1f}".into(),
                "directory-resource\u{1f}0\u{1f}resource:first".into(),
                super::job_status_id(7),
                super::job_cancel_id(7),
            ] {
                assert!(
                    provider.action_hierarchy(&id).is_none(),
                    "not an application action: {id:?}"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the authenticated directory traversal scenario is clearest as one test"
    )]
    fn live_actions_follow_the_authenticated_resource_room_and_scope() {
        // Phase `live_action_proximity_uses_the_authenticated_resource_room`.
        {
            let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
            let provider = LiveAppActionsProvider::new(broker);
            let chrome = chrome();
            let chrome = chrome.borrow();
            let room = chrome.tree().desktop.workspaces[0].room.to_string();
            let result = |cluster: &str| SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: PROVIDER_ID.into(),
                id: [
                    "so.tilo.files",
                    "write",
                    cluster,
                    "file:/Brief.md",
                    "files.entry:/Brief.md",
                    "rename",
                ]
                .join(&ID_SEPARATOR.to_string()),
                title: "Rename".into(),
                subtitle: None,
                badge: None,
                score: 100,
                params: Vec::new(),
                contract: None,
                on_open: Rc::new(|_, _, _, _| Ok(())),
                on_invoke: None,
                on_begin_invoke: None,
            };

            assert_eq!(
                provider.proximity(&result(&room), &chrome),
                SearchProximity::CurrentWorkspace
            );
            assert_eq!(
                provider.proximity(&result("another-room"), &chrome),
                SearchProximity::Global
            );
        }

        // Phase `resource_only_directory_row_enters_an_authenticated_child_scope`.
        {
            let chrome = chrome();
            let broker = broker_for(&chrome);
            let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
            let cluster_id = chrome.borrow().tree().desktop.workspaces[0]
                .room
                .to_string();
            let actor = chrome
                .borrow()
                .local_user_for_room(&chrome.borrow().tree().desktop.workspaces[0].room);
            let root = ActionDirectoryRoot {
                peer_id: 9,
                app_id: "so.tilo.reader".into(),
                cluster_id: cluster_id.clone(),
                record_id: "library".into(),
            };
            broker.replace_action_directory_roots_for_test(vec![root]);
            provider.directory_pages.borrow_mut().insert(
                (9, actor, "book".into(), Vec::new()),
                page(
                    vec![CatalogueRow {
                        provider: "9".into(),
                        app_id: "so.tilo.reader".into(),
                        parent_resource: String::new(),
                        resource: CatalogueResource {
                            id: "book:42".into(),
                            kind: "book".into(),
                            label: "Book of Tilo".into(),
                            presentation: Some(tilo_actions::ResourcePresentation {
                                detail: Some("Chapter 7".into()),
                                badge: Some("2 collaborators".into()),
                                preview: None,
                            }),
                        },
                        action: None,
                        has_children: true,
                        fresh_binding: None,
                        visibility: None,
                    }],
                    Vec::new(),
                ),
            );
            let doc = AutoCommit::new();
            let wanted = [ResourceKind::Custom("Resources".into())];
            let chrome_ref = chrome.borrow();
            let page = provider.query_page(
                SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "book",
                    wanted: &wanted,
                    chrome: &chrome_ref,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                },
                None,
                SearchBudget::PALETTE,
            );
            drop(chrome_ref);
            assert_eq!(page.results.len(), 1);
            assert_eq!(page.results[0].title, "Book of Tilo");
            assert_eq!(
                page.results[0].subtitle.as_deref(),
                Some("so.tilo.reader · Chapter 7")
            );
            assert_eq!(page.results[0].badge.as_deref(), Some("2 collaborators"));
            (page.results[0].on_open)(
                &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
                &chrome,
                &crate::shell::search::action_api::ActionArgs::none(),
            )
            .unwrap();
            assert_eq!(
                chrome.borrow().search_resource_scope,
                Some(crate::shell::chrome_state::SearchResourceScope {
                    peer_id: 9,
                    cluster_id,
                    app_id: "so.tilo.reader".into(),
                    resource: "book:42".into(),
                    label: "Book of Tilo".into(),
                    kind: "book".into(),
                })
            );
            let chrome_ref = chrome.borrow();
            let _ = provider.query_page(
                SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "",
                    wanted: &wanted,
                    chrome: &chrome_ref,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                },
                None,
                SearchBudget::PALETTE,
            );
            assert_eq!(
                provider
                    .directory_query_probe
                    .borrow()
                    .as_ref()
                    .and_then(|query| query.parent_resource.as_deref()),
                Some("book:42"),
                "the child fetch must carry the selected stable resource address"
            );
        }
    }
}
