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

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::ActionReceipt;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{
    InvocationDisposition, PendingInvocation, ResourceKind, SearchBudget, SearchContext,
    SearchCursor, SearchPage, SearchProvider, SearchProximity, SearchResult,
};

use crate::shell::collab_broker_capnp::{
    ActionDirectoryBridgeResponse, ActionDirectoryRequestQueue, BrokerActor,
    DirectoryActionConfirmation, PublishedAction, PublishedInput, PublishedSurface,
};

pub const PROVIDER_ID: &str = "app-actions";
const ID_SEPARATOR: char = '\u{1f}';
const MAX_DIRECTORY_ROOTS_PER_QUERY: usize = 16;
/// Jobs are an immediate, useful palette hint rather than a second unbounded
/// search corpus.  They are deliberately a small first stage before remote
/// and local actions.
const MAX_JOB_ROWS_PER_QUERY: usize = 4;
/// Directory responses are an acceleration cache, not a second unbounded
/// replica of every connected application's action namespace.
const MAX_RETAINED_DIRECTORY_PAGES: usize = 64;
/// A stalled LocalSet/capability must not leave a palette-owned oneshot alive
/// forever. Retrying after this deadline remains bounded by the bridge queue.
const DIRECTORY_PAGE_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// One remote directory page is addressed by its certified route, actor,
/// query, and opaque remote continuation. Keeping the continuation in the
/// key lets a bounded palette walk a long directory without replacing an
/// already-rendered page.
type DirectoryCacheKey = (u64, String, String, Vec<u8>);

struct PendingDirectoryPage {
    receiver: tokio::sync::oneshot::Receiver<
        crate::shell::collab_broker_capnp::ActionDirectoryBridgeReply,
    >,
    deadline: Instant,
    /// The first page changes the visible source and stales old cursors. A
    /// later page merely appends behind an existing continuation, which keeps
    /// that continuation valid and avoids replaying earlier remote rows.
    initial_page: bool,
}

fn invalidates_directory_cache(
    error: crate::shell::collab_broker_capnp::ActionDirectoryBridgeError,
) -> bool {
    matches!(
        error,
        crate::shell::collab_broker_capnp::ActionDirectoryBridgeError::StaleRevision
            | crate::shell::collab_broker_capnp::ActionDirectoryBridgeError::NotFound
            | crate::shell::collab_broker_capnp::ActionDirectoryBridgeError::Unavailable
    )
}

fn directory_start_ticket(
    receiver: tokio::sync::oneshot::Receiver<
        crate::shell::collab_broker_capnp::ActionDirectoryBridgeReply,
    >,
    invalidations: Rc<RefCell<HashSet<DirectoryCacheKey>>>,
    cache_key: DirectoryCacheKey,
    deadline: Instant,
) -> PendingInvocation {
    let receiver = Rc::new(RefCell::new(receiver));
    Rc::new(move || {
        let reply = receiver.borrow_mut().try_recv();
        match reply {
            Ok(Ok(ActionDirectoryBridgeResponse::StartedDirectoryAction(started))) => {
                Ok(Some(ActionReceipt {
                    broker_invocation_id: Some(started.invocation_id),
                    update: started.initial_update,
                }))
            }
            Ok(Err(error)) => {
                if invalidates_directory_cache(error) {
                    invalidations.borrow_mut().insert(cache_key.clone());
                }
                Err(ActionError::Refused(format!(
                    "directory action refused: {error:?}"
                )))
            }
            Ok(Ok(_)) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(
                ActionError::Refused("directory action response was unavailable".into()),
            ),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) if Instant::now() >= deadline => {
                receiver.borrow_mut().close();
                invalidations.borrow_mut().insert(cache_key.clone());
                Err(ActionError::Refused(
                    "directory action admission timed out".into(),
                ))
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(None),
        }
    })
}

pub fn job_status_id(id: u64) -> String {
    format!("job{ID_SEPARATOR}{id}")
}

pub fn job_cancel_id(id: u64) -> String {
    format!("job-cancel{ID_SEPARATOR}{id}")
}

pub struct LiveAppActionsProvider {
    broker: AppActionBroker,
    directory_queue: ActionDirectoryRequestQueue,
    directory_pages: RefCell<HashMap<DirectoryCacheKey, runtime_collab::ActionDirectoryPage>>,
    directory_pending: RefCell<HashMap<DirectoryCacheKey, PendingDirectoryPage>>,
    /// The palette owns at most one needle's directory projection.
    /// Keeping previous query terms made rapid typing retain unbounded pages
    /// and receivers even though no result could ever be rendered again.
    directory_query: RefCell<Option<String>>,
    resource_kinds: Vec<ResourceKind>,
    directory_invalidations: Rc<RefCell<HashSet<DirectoryCacheKey>>>,
    directory_epoch: AtomicU64,
    surface_snapshot: RefCell<Option<SurfaceSnapshot>>,
    surface_build: RefCell<Option<Receiver<SurfaceSnapshot>>>,
    auxiliary_snapshot: RefCell<Option<AuxiliarySnapshot>>,
    projection: Cell<QueryProjection>,
    #[cfg(test)]
    build_probe: Arc<BuildProbe>,
    #[cfg(test)]
    directory_query_probe: RefCell<Option<runtime_collab::ActionDirectoryQuery>>,
}

struct SurfaceSnapshot {
    source_generation: u64,
    scope: String,
    entries: Vec<(PublishedSurface, PublishedAction)>,
    index: ui::search::SearchTextIndex,
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
        root: crate::shell::collab_broker_capnp::ActionDirectoryRoot,
        revision: String,
        descriptor: runtime_collab::ExportedAction,
        actor: String,
        cursor: Vec<u8>,
    },
    Resource {
        root: crate::shell::collab_broker_capnp::ActionDirectoryRoot,
        row: runtime_collab::ActionDirectoryRow,
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

struct PeerInvocation<'a> {
    peer_id: u64,
    resource: &'a str,
    action: &'a str,
    argument: Option<&'a str>,
    actor: &'a str,
    confirmation: &'a tilo_actions::ConfirmationPolicy,
    confirmed: bool,
    phrase: Option<&'a str>,
}

fn start_on_peer(
    broker: &AppActionBroker,
    invocation: &PeerInvocation<'_>,
) -> Result<crate::shell::collab_broker_capnp::InvocationJob, String> {
    match invocation.confirmation {
        tilo_actions::ConfirmationPolicy::None => broker.start(
            invocation.peer_id,
            invocation.resource,
            invocation.action,
            invocation.argument,
            invocation.actor,
        ),
        tilo_actions::ConfirmationPolicy::Confirm => {
            if !invocation.confirmed {
                return Err("explicit confirmation is required".into());
            }
            let token = broker.prepare_confirmation(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                None,
            )?;
            broker.start_confirmed(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                &token,
            )
        }
        tilo_actions::ConfirmationPolicy::Phrase { .. } => {
            let token = broker.prepare_confirmation(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                invocation.phrase,
            )?;
            broker.start_confirmed(
                invocation.peer_id,
                invocation.resource,
                invocation.action,
                invocation.argument,
                invocation.actor,
                &token,
            )
        }
    }
}

fn deadline_after(duration: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(duration).unwrap_or(now)
}

fn receipt_for_job(
    broker: &AppActionBroker,
    job: crate::shell::collab_broker_capnp::InvocationJob,
) -> ActionReceipt {
    let update =
        broker
            .status(job.id)
            .unwrap_or_else(|| tilo_actions::InvocationUpdate::Accepted {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job.id))),
            });
    drop(job.completion);
    ActionReceipt {
        broker_invocation_id: Some(job.id),
        update,
    }
}

fn invocation_argument(
    args: &crate::shell::search::action_api::ActionArgs,
    typed_arguments: bool,
    wants_content: bool,
) -> (Option<String>, bool, Option<String>) {
    let encoded = typed_arguments.then(|| {
        let mut application_args = args.to_json();
        application_args.remove("confirm");
        application_args.remove("confirmation_phrase");
        format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(application_args)
        )
    });
    let argument = encoded.or_else(|| {
        if wants_content {
            args.text("content").map(str::to_owned)
        } else {
            args.text("argument").map(str::to_owned)
        }
    });
    (
        argument,
        args.boolean("confirm") == Some(true),
        args.text("confirmation_phrase").map(str::to_owned),
    )
}

impl LiveAppActionsProvider {
    fn wants_resource_rows(wanted: &[ResourceKind]) -> bool {
        wanted.is_empty()
            || wanted
                .iter()
                .any(|kind| matches!(kind, ResourceKind::Custom(label) if label == "Resources"))
    }

    pub fn new(broker: AppActionBroker) -> Self {
        let directory_queue = broker.action_directory_request_queue();
        Self {
            broker,
            directory_queue,
            directory_pages: RefCell::new(HashMap::new()),
            directory_pending: RefCell::new(HashMap::new()),
            directory_query: RefCell::new(None),
            resource_kinds: vec![
                ResourceKind::Action,
                ResourceKind::Custom("Resources".into()),
            ],
            directory_invalidations: Rc::new(RefCell::new(HashSet::new())),
            directory_epoch: AtomicU64::new(0),
            surface_snapshot: RefCell::new(None),
            surface_build: RefCell::new(None),
            auxiliary_snapshot: RefCell::new(None),
            projection: Cell::new(QueryProjection::All),
            #[cfg(test)]
            build_probe: Arc::new(BuildProbe::default()),
            #[cfg(test)]
            directory_query_probe: RefCell::new(None),
        }
    }

    fn surface_scope(ctx: &SearchContext<'_>) -> String {
        let room = ctx
            .chrome
            .tree()
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
            ctx.chrome.search_scope
        )
    }

    /// Return only indexed local actions in the current scope.  The broker
    /// inventory is cloned and indexed only after its generation changes;
    /// ordinary palette edits inspect a bounded posting instead.
    fn build_scope(ctx: &SearchContext<'_>) -> SurfaceBuildScope {
        let room = ctx
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        let app = ctx.chrome.active_app_id();
        SurfaceBuildScope {
            key: Self::surface_scope(ctx),
            scope: ctx.chrome.search_scope,
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
                actions
                    .into_iter()
                    .map(move |action| (surface.clone(), action))
            })
            .collect::<Vec<_>>();
        let index = ui::search::SearchTextIndex::new(entries.iter().map(|(surface, action)| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                action.action_label,
                action.resource_label,
                action.action,
                action.resource,
                surface.app_id
            )
        }));
        SurfaceSnapshot {
            source_generation,
            scope: scope.key.clone(),
            entries,
            index,
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
            let broker = self.broker.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            if std::thread::Builder::new()
                .name("tilo-live-action-index".into())
                .spawn({
                    #[cfg(test)]
                    let probe = Arc::clone(&self.build_probe);
                    move || {
                        let _ = sender.send(Self::build_local_snapshot(
                            &broker,
                            &scope,
                            #[cfg(test)]
                            &probe,
                        ));
                    }
                })
                .is_ok()
            {
                *self.surface_build.borrow_mut() = Some(receiver);
            }
        }
    }

    fn local_action_candidates(
        &self,
        needle: &str,
        max_results: usize,
    ) -> Vec<(PublishedSurface, PublishedAction)> {
        let snapshot = self.surface_snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            return Vec::new();
        };
        let page = snapshot
            .index
            .matching_page(needle, None, max_results, max_results);
        page.ids
            .into_iter()
            .filter_map(|id| snapshot.entries.get(id).cloned())
            .collect()
    }

    /// Reuse the established action-row constructor for one already-indexed
    /// action.  The temporary one-row projection keeps all typed callbacks,
    /// contracts, confirmation handling, and archived-launch behaviour in
    /// precisely one implementation while `query_page` accounts for this
    /// candidate explicitly.
    fn materialize_local_action(
        &self,
        ctx: &SearchContext<'_>,
        surface: PublishedSurface,
        action: PublishedAction,
    ) -> Option<SearchResult> {
        let id = Self::result_id(&surface, &action);
        let scope = Self::surface_scope(ctx);
        let source_generation = self.broker.surfaces_generation();
        let text = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            action.action_label,
            action.resource_label,
            action.action,
            action.resource,
            surface.app_id
        );
        let temporary = SurfaceSnapshot {
            source_generation,
            scope,
            entries: vec![(surface, action)],
            index: ui::search::SearchTextIndex::new([text]),
        };
        let previous = self.surface_snapshot.replace(Some(temporary));
        let previous_projection = self.projection.replace(QueryProjection::LocalOnly);
        let results = self.query(SearchContext {
            needle: ctx.needle,
            wanted: ctx.wanted,
            chrome: ctx.chrome,
            doc: ctx.doc,
            doc_root: ctx.doc_root,
        });
        self.projection.set(previous_projection);
        self.surface_snapshot.replace(previous);
        results.into_iter().find(|result| result.id == id)
    }

    fn poll_directory_pages_at(&self, now: Instant) {
        let mut ready = Vec::new();
        self.directory_pending.borrow_mut().retain(|key, pending| {
            if pending.deadline <= now {
                return false;
            }
            match pending.receiver.try_recv() {
                Ok(Ok(ActionDirectoryBridgeResponse::Page(page))) if !page.stale_cursor => {
                    ready.push((key.clone(), page, pending.initial_page));
                    false
                }
                Ok(Err(_) | Ok(_)) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    false
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true,
            }
        });
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
        let root_page = self.broker.action_directory_root_page(
            resume.root_after,
            root_limit.min(MAX_DIRECTORY_ROOTS_PER_QUERY),
        );
        let root_next_after = root_page.next_after_peer;
        let mut roots = resume
            .directory
            .as_ref()
            .map(|directory| vec![directory.root.clone()])
            .unwrap_or(root_page.roots);
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
                let state = match update {
                    tilo_actions::InvocationUpdate::Accepted { .. } => "accepted",
                    tilo_actions::InvocationUpdate::Progress { .. } => "running",
                    tilo_actions::InvocationUpdate::Item { .. } => "streaming",
                    tilo_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
                    tilo_actions::InvocationUpdate::Failed { .. } => "failed",
                    tilo_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
                };
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
                ctx.chrome.search_scope,
                crate::shell::chrome_state::SearchScope::All
            ) && ctx
                .chrome
                .tree()
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
                != Some(root.cluster_id.clone())
            {
                continue;
            }
            let root_actor = ctx
                .chrome
                .tree()
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
                    if let Some(descriptor) = row.descriptor.clone() {
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

    fn job_result(&self, id: u64, state: &'static str, cancel: bool, actor: &str) -> SearchResult {
        if cancel {
            let broker = self.broker.clone();
            let actor = actor.to_string();
            let invoke_broker = broker.clone();
            let invoke_actor = actor.clone();
            return SearchResult {
                kind: ResourceKind::Action,
                provider_id: PROVIDER_ID.into(),
                id: job_cancel_id(id),
                title: format!("Cancel action job {id}"),
                subtitle: Some("Requests cooperative cancellation".into()),
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
                    let update = invoke_broker
                        .status(id)
                        .ok_or_else(|| ActionError::Refused("unknown invocation id".into()))?;
                    Ok(ActionReceipt {
                        broker_invocation_id: Some(id),
                        update,
                    })
                })),
                on_begin_invoke: None,
            };
        }
        let status_broker = self.broker.clone();
        SearchResult {
            kind: ResourceKind::Custom("action-job".into()),
            provider_id: PROVIDER_ID.into(),
            id: job_status_id(id),
            title: format!("Action job {id}"),
            subtitle: Some(state.into()),
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
        }
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
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: String::new(),
            cursor: cursor.to_vec(),
            needle: needle.to_string(),
            parent_resource: parent_resource.to_string(),
            max_results: 48,
            max_bytes: 48 * 1024,
            max_work: 4_096,
            max_depth: 1,
        };
        #[cfg(test)]
        self.directory_query_probe.replace(Some(query.clone()));
        if let Ok(receiver) =
            self.directory_queue
                .try_query(BrokerActor::person(actor), cluster_id, query)
        {
            self.directory_pending.borrow_mut().insert(
                key,
                PendingDirectoryPage {
                    receiver,
                    deadline: deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
                    initial_page: cursor.is_empty(),
                },
            );
        }
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
    ) -> Option<(&str, tilo_actions::PermissionRequirement, &str)> {
        let (app, permission, _cluster, _record, _resource, action) = Self::result_parts(id)?;
        let permission = match permission {
            "none" => tilo_actions::PermissionRequirement::None,
            "read" => tilo_actions::PermissionRequirement::Read,
            "comment" => tilo_actions::PermissionRequirement::Comment,
            "write" => tilo_actions::PermissionRequirement::Write,
            "invite" => tilo_actions::PermissionRequirement::Invite,
            "admin" => tilo_actions::PermissionRequirement::Admin,
            _ => return None,
        };
        Some((app, permission, action))
    }

    /// The application, room, record, resource, and operation represented by
    /// one provider-local id.
    ///
    /// The palette needs only the opaque id. Headless integrations also need
    /// its existing resource hierarchy so they can reveal one application,
    /// then one object, without flattening every action into their initial
    /// context. Keep the decoding beside [`Self::result_id`], where the wire
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
            match action.contract.permission {
                tilo_actions::PermissionRequirement::None => "none",
                tilo_actions::PermissionRequirement::Read => "read",
                tilo_actions::PermissionRequirement::Comment => "comment",
                tilo_actions::PermissionRequirement::Write => "write",
                tilo_actions::PermissionRequirement::Invite => "invite",
                tilo_actions::PermissionRequirement::Admin => "admin",
            },
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
        match &action.contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => {}
            tilo_actions::ConfirmationPolicy::Confirm => params.push(ActionParam::required(
                "confirm",
                ParamType::Boolean,
                "Explicitly confirm this consequential action",
            )),
            tilo_actions::ConfirmationPolicy::Phrase { .. } => {
                params.push(ActionParam::required(
                    "confirmation_phrase",
                    ParamType::Text,
                    "Type the action's confirmation phrase exactly",
                ));
            }
        }
        params
    }

    fn directory_action_result(
        &self,
        root: &crate::shell::collab_broker_capnp::ActionDirectoryRoot,
        revision: &str,
        descriptor: &runtime_collab::ExportedAction,
        needle: &str,
        actor: &str,
        cursor: &[u8],
    ) -> Option<SearchResult> {
        let searchable = format!(
            "{} {} {} {}",
            descriptor.action_label, descriptor.resource_label, descriptor.action, root.app_id
        )
        .to_lowercase();
        let score = if needle.is_empty() {
            0
        } else if descriptor.action_label.to_lowercase().starts_with(needle) {
            100
        } else if searchable.contains(needle) {
            50
        } else {
            return None;
        };
        let contract =
            serde_json::from_str::<tilo_actions::ActionContract>(&descriptor.contract_json).ok()?;
        let mut params = serde_json::from_str::<Vec<ActionParam>>(&descriptor.params_json).ok()?;
        match &contract.confirmation {
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
        let queue = self.directory_queue.clone();
        let peer_id = root.peer_id;
        let cluster_id = root.cluster_id.clone();
        let revision = revision.to_string();
        let resource = descriptor.resource.clone();
        let action = descriptor.action.clone();
        let actor = actor.to_string();
        let invalidations = Rc::clone(&self.directory_invalidations);
        let cache_key = (peer_id, actor.clone(), needle.to_string(), cursor.to_vec());
        let typed =
            !descriptor.params_json.trim().is_empty() && descriptor.params_json.trim() != "[]";
        Some(SearchResult {
            kind: ResourceKind::Action,
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
                    Some(format!(
                        "{}{}",
                        tilo_actions::ARGUMENTS_JSON_PREFIX,
                        serde_json::Value::Object(values)
                    ))
                } else {
                    args.text("argument").map(str::to_owned)
                };
                let confirmation = if args.boolean("confirm") == Some(true) {
                    Some(DirectoryActionConfirmation::Confirmed)
                } else {
                    args.text("confirmation_phrase")
                        .map(|value| DirectoryActionConfirmation::Phrase(value.to_owned()))
                };
                let receiver = queue
                    .try_start_resolved(
                        BrokerActor::person(&actor),
                        &cluster_id,
                        peer_id,
                        &revision,
                        &resource,
                        &action,
                        argument,
                        confirmation,
                    )
                    .map_err(|error| {
                        ActionError::Refused(format!("directory action unavailable: {error:?}"))
                    })?;
                let ticket = directory_start_ticket(
                    receiver,
                    Rc::clone(&invalidations),
                    cache_key.clone(),
                    deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
                );
                Ok(InvocationDisposition::Pending(ticket))
            })),
        })
    }

    fn directory_resource_result(
        root: &crate::shell::collab_broker_capnp::ActionDirectoryRoot,
        row: &runtime_collab::ActionDirectoryRow,
        needle: &str,
    ) -> Option<SearchResult> {
        let presentation = serde_json::from_str::<tilo_actions::ResourcePresentation>(
            &row.resource_presentation_json,
        )
        .ok()
        .filter(tilo_actions::ResourcePresentation::is_valid);
        let presentation_text = presentation
            .as_ref()
            .into_iter()
            .flat_map(|value| {
                [
                    value.detail.as_deref(),
                    value.badge.as_deref(),
                    value.preview.as_deref(),
                ]
            })
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let searchable = format!(
            "{} {} {}",
            row.resource_label, row.resource, presentation_text
        )
        .to_lowercase();
        let score = if needle.is_empty() {
            0
        } else if row.resource_label.to_lowercase().starts_with(needle) {
            100
        } else if searchable.contains(needle) {
            50
        } else {
            return None;
        };
        let scope = crate::shell::chrome_state::SearchResourceScope {
            peer_id: root.peer_id,
            cluster_id: root.cluster_id.clone(),
            app_id: root.app_id.clone(),
            resource: row.resource.clone(),
            label: row.resource_label.clone(),
        };
        let title = row.resource_label.clone();
        Some(SearchResult {
            kind: ResourceKind::Custom(if row.resource_kind.is_empty() {
                "Resources".into()
            } else {
                row.resource_kind.clone()
            }),
            provider_id: PROVIDER_ID.into(),
            id: format!(
                "directory-resource{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
                root.peer_id, row.resource
            ),
            title,
            subtitle: Some(
                presentation
                    .as_ref()
                    .and_then(|value| value.detail.as_deref())
                    .map_or_else(
                        || format!("{} · contains resources", root.app_id),
                        |detail| format!("{} · {detail}", root.app_id),
                    ),
            ),
            badge: presentation.and_then(|value| value.badge),
            score,
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
        })
    }
}

impl SearchProvider for LiveAppActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Application resources and actions"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.resource_kinds
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
            && result.id.starts_with("directory\u{1f}")
        {
            return SearchProximity::CurrentWorkspace;
        }
        SearchProximity::Global
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the query constructs paired palette/headless callbacks from one captured action contract; splitting them would duplicate or desynchronise confirmation, launch, and job lifecycle state"
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
            self.poll_directory_pages();
        }
        if self.projection.get() != QueryProjection::LocalOnly {
            for root in self
                .broker
                .action_directory_root_page(None, MAX_DIRECTORY_ROOTS_PER_QUERY)
                .roots
            {
                if let Some(resource_scope) = &ctx.chrome.search_resource_scope
                    && (root.peer_id != resource_scope.peer_id
                        || root.cluster_id != resource_scope.cluster_id
                        || root.app_id != resource_scope.app_id)
                {
                    continue;
                }
                if !matches!(
                    ctx.chrome.search_scope,
                    crate::shell::chrome_state::SearchScope::All
                ) && ctx
                    .chrome
                    .tree()
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
                        if let Some(descriptor) = row.descriptor
                            && let Some(result) = self.directory_action_result(
                                &root,
                                &page.revision,
                                &descriptor,
                                &needle,
                                &root_actor,
                                &[],
                            )
                        {
                            results.push(result);
                        }
                    }
                }
            }
        }
        if self.projection.get() != QueryProjection::LocalOnly
            && (needle.is_empty() || "jobs status progress".contains(&needle))
        {
            for (id, update) in self.broker.statuses(&actor) {
                let state = match &update {
                    tilo_actions::InvocationUpdate::Accepted { .. } => "accepted",
                    tilo_actions::InvocationUpdate::Progress { .. } => "running",
                    tilo_actions::InvocationUpdate::Item { .. } => "streaming",
                    tilo_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
                    tilo_actions::InvocationUpdate::Failed { .. } => "failed",
                    tilo_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
                };
                let status_broker = self.broker.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Custom("action-job".into()),
                    provider_id: PROVIDER_ID.into(),
                    id: job_status_id(id),
                    title: format!("Action job {id}"),
                    subtitle: Some(state.into()),
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
                        provider_id: PROVIDER_ID.into(),
                        id: job_cancel_id(id),
                        title: format!("Cancel action job {id}"),
                        subtitle: Some("Requests cooperative cancellation".into()),
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
            for (surface, owned_action) in
                self.local_action_candidates(&needle, SearchBudget::PALETTE.max_results)
            {
                let action = &owned_action;
                let unavailable_reason = match &action.contract.availability {
                    tilo_actions::Availability::Available => None,
                    tilo_actions::Availability::Unavailable { reason, .. } => Some(reason.clone()),
                };
                let searchable = format!(
                    "{} {} {} {} {}",
                    action.action_label,
                    action.resource_label,
                    action.action,
                    action.resource,
                    surface.app_id
                )
                .to_lowercase();
                let score = if needle.is_empty() {
                    0
                } else if action.action_label.to_lowercase().starts_with(&needle) {
                    100
                } else if searchable.contains(&needle) {
                    50
                } else {
                    continue;
                };
                let params = Self::action_params(action);
                let broker = self.broker.clone();
                let peer_id = surface.peer_id;
                let resource = action.resource.clone();
                let action_id = action.action.clone();
                let actor = actor.clone();
                let wants_content = matches!(
                    action.input,
                    PublishedInput::Text {
                        wants_content: true,
                        ..
                    }
                );
                let typed_arguments = !action.params.is_empty();
                let confirmation = action.contract.confirmation.clone();
                let surface_live = surface.live;
                let app_id = surface.app_id.clone();
                let cluster_id = surface.route.cluster_id.clone();
                let action_label = action.action_label.clone();
                let invoke_broker = broker.clone();
                let invoke_resource = resource.clone();
                let invoke_action_id = action_id.clone();
                let invoke_actor = actor.clone();
                let invoke_confirmation = confirmation.clone();
                let invoke_app_id = app_id.clone();
                let invoke_cluster_id = cluster_id.clone();
                let invoke_unavailable_reason = unavailable_reason.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: PROVIDER_ID.to_string(),
                    id: Self::result_id(&surface, action),
                    title: action.action_label.clone(),
                    subtitle: Some(unavailable_reason.clone().unwrap_or_else(|| {
                        action.description.clone().unwrap_or_else(|| {
                            if surface_live {
                                format!("{} · {}", action.resource_label, surface.app_id)
                            } else {
                                format!(
                                    "{} · {} · launches application",
                                    action.resource_label, surface.app_id
                                )
                            }
                        })
                    })),
                    badge: None,
                    score,
                    params,
                    contract: Some(action.contract.clone()),
                    on_open: std::rc::Rc::new(move |_doc, _root, chrome, args| {
                        // Availability on an archived launchable descriptor is
                        // only a last-known hint. Launch first and let the
                        // freshly published surface decide; otherwise an app
                        // closed while temporarily busy could never be opened
                        // through that action again.
                        if surface_live && let Some(reason) = &unavailable_reason {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let encoded_arguments = typed_arguments.then(|| {
                            let mut application_args = args.to_json();
                            application_args.remove("confirm");
                            application_args.remove("confirmation_phrase");
                            format!(
                                "{}{}",
                                tilo_actions::ARGUMENTS_JSON_PREFIX,
                                serde_json::Value::Object(application_args)
                            )
                        });
                        let argument = if let Some(arguments) = encoded_arguments.as_deref() {
                            Some(arguments)
                        } else if wants_content {
                            args.text("content")
                        } else {
                            args.text("argument")
                        };
                        let confirmed = args.boolean("confirm") == Some(true);
                        let phrase = args.text("confirmation_phrase").map(str::to_owned);
                        if !surface_live {
                            // Validate the human confirmation before starting
                            // another process; the broker binds a fresh one-use
                            // grant to the reconnected peer immediately before
                            // delivery.
                            match &confirmation {
                                tilo_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                tilo_actions::ConfirmationPolicy::Phrase { phrase: expected }
                                    if phrase.as_deref() != Some(expected.as_str()) =>
                                {
                                    return Err(ActionError::Refused(
                                        "the confirmation phrase did not match".into(),
                                    ));
                                }
                                _ => {}
                            }
                            crate::shell::search::providers::applications::launch_installed_app(
                                chrome, &app_id, &app_id,
                            )?;
                            let broker = broker.clone();
                            let resource = resource.clone();
                            let action_id = action_id.clone();
                            let argument = argument.map(str::to_owned);
                            let actor = actor.clone();
                            let confirmation = confirmation.clone();
                            let app_id = app_id.clone();
                            let cluster_id = cluster_id.clone();
                            let action_label = action_label.clone();
                            std::thread::Builder::new()
                                .name("tilo-deferred-action".into())
                                .spawn(move || {
                                    let deadline =
                                        deadline_after(std::time::Duration::from_secs(30));
                                    while std::time::Instant::now() < deadline {
                                        let target =
                                            broker.surfaces().into_iter().find(|surface| {
                                                surface.live
                                                    && surface.app_id == app_id
                                                    && surface.route.cluster_id == cluster_id
                                                    && surface.actions.iter().any(|candidate| {
                                                        candidate.resource == resource
                                                            && candidate.action == action_id
                                                    })
                                            });
                                        if let Some(target) = target {
                                            match start_on_peer(
                                                &broker,
                                                &PeerInvocation {
                                                    peer_id: target.peer_id,
                                                    resource: &resource,
                                                    action: &action_id,
                                                    argument: argument.as_deref(),
                                                    actor: &actor,
                                                    confirmation: &confirmation,
                                                    confirmed,
                                                    phrase: phrase.as_deref(),
                                                },
                                            ) {
                                                Ok(job) => {
                                                    tracing::debug!(
                                                        invocation_id = job.id,
                                                        "deferred application action accepted"
                                                    );
                                                    drop(job.completion);
                                                }
                                                Err(error) => tracing::warn!(
                                                    %error,
                                                    action = %action_label,
                                                    "deferred application action was refused"
                                                ),
                                            }
                                            return;
                                        }
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                    tracing::warn!(
                                        action = %action_label,
                                        app = %app_id,
                                        "application did not publish its launchable action in time"
                                    );
                                })
                                .map_err(|error| ActionError::Refused(error.to_string()))?;
                            return Ok(());
                        }
                        let job = start_on_peer(
                            &broker,
                            &PeerInvocation {
                                peer_id,
                                resource: &resource,
                                action: &action_id,
                                argument,
                                actor: &actor,
                                confirmation: &confirmation,
                                confirmed,
                                phrase: phrase.as_deref(),
                            },
                        )
                        .map_err(ActionError::Refused)?;
                        tracing::debug!(invocation_id = job.id, "application action accepted");
                        // Completion remains owned by the broker's job registry;
                        // dropping this compatibility receiver does not erase status.
                        drop(job.completion);
                        // Admission is synchronous; execution is observed as a
                        // broker job rather than blocking the compositor.
                        Ok(())
                    }),
                    on_invoke: Some(std::rc::Rc::new(move |_doc, _root, chrome, args| {
                        if surface_live && let Some(reason) = &invoke_unavailable_reason {
                            return Err(ActionError::Refused(reason.clone()));
                        }
                        let (argument, confirmed, phrase) =
                            invocation_argument(args, typed_arguments, wants_content);
                        if !surface_live {
                            match &invoke_confirmation {
                                tilo_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                tilo_actions::ConfirmationPolicy::Phrase { phrase: expected }
                                    if phrase.as_deref() != Some(expected.as_str()) =>
                                {
                                    return Err(ActionError::Refused(
                                        "the confirmation phrase did not match".into(),
                                    ));
                                }
                                _ => {}
                            }
                            crate::shell::search::providers::applications::launch_installed_app(
                                chrome,
                                &invoke_app_id,
                                &invoke_app_id,
                            )?;
                            let deadline = deadline_after(std::time::Duration::from_secs(30));
                            while std::time::Instant::now() < deadline {
                                let target = invoke_broker.surfaces().into_iter().find(|surface| {
                                    surface.live
                                        && surface.app_id == invoke_app_id
                                        && surface.route.cluster_id == invoke_cluster_id
                                        && surface.actions.iter().any(|candidate| {
                                            candidate.resource == invoke_resource
                                                && candidate.action == invoke_action_id
                                        })
                                });
                                if let Some(target) = target {
                                    let job = start_on_peer(
                                        &invoke_broker,
                                        &PeerInvocation {
                                            peer_id: target.peer_id,
                                            resource: &invoke_resource,
                                            action: &invoke_action_id,
                                            argument: argument.as_deref(),
                                            actor: &invoke_actor,
                                            confirmation: &invoke_confirmation,
                                            confirmed,
                                            phrase: phrase.as_deref(),
                                        },
                                    )
                                    .map_err(ActionError::Refused)?;
                                    return Ok(receipt_for_job(&invoke_broker, job));
                                }
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            return Err(ActionError::Refused(
                                "application did not publish its launchable action in time".into(),
                            ));
                        }
                        let job = start_on_peer(
                            &invoke_broker,
                            &PeerInvocation {
                                peer_id,
                                resource: &invoke_resource,
                                action: &invoke_action_id,
                                argument: argument.as_deref(),
                                actor: &invoke_actor,
                                confirmation: &invoke_confirmation,
                                confirmed,
                                phrase: phrase.as_deref(),
                            },
                        )
                        .map_err(ActionError::Refused)?;
                        Ok(receipt_for_job(&invoke_broker, job))
                    })),
                    on_begin_invoke: None,
                });
            }
        }
        results
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
            "{}:{source_generation}:{}:{}",
            if ready { "ready" } else { "indexing" },
            self.directory_epoch.load(Ordering::Relaxed),
            // A zero-length retained root page is a generation read only;
            // it neither clones a route nor fans out a directory request.
            self.broker.action_directory_root_page(None, 0).generation,
        )
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
        let needle = ctx.needle.trim().to_lowercase();
        self.set_directory_context(&ctx, &needle);
        self.apply_directory_invalidations();
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
                        if !ctx.wanted.is_empty() && !ctx.wanted.contains(&ResourceKind::Action) {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        if let Some(row) = self.directory_action_result(
                            root, revision, descriptor, &needle, actor, cursor,
                        ) {
                            row
                        } else {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                    }
                    AuxiliaryEntry::Resource { root, row } => {
                        if !Self::wants_resource_rows(ctx.wanted) {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
                        if let Some(row) = Self::directory_resource_result(root, row, &needle) {
                            row
                        } else {
                            position = position.saturating_add(1);
                            work = work.saturating_add(1);
                            continue;
                        }
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

        let local_after = (stage == "local" && after != usize::MAX).then_some(after);
        let local_limit = budget.max_results.saturating_sub(results.len());
        let local_work = budget.max_work.saturating_sub(work);
        if local_limit == 0 || local_work == 0 {
            return Self::live_page(
                revision,
                results,
                "local",
                local_after.unwrap_or(usize::MAX),
                &needle,
                &scope,
            );
        }
        let page = {
            let snapshot = self.surface_snapshot.borrow();
            let Some(snapshot) = snapshot.as_ref() else {
                return SearchPage::exhausted(revision, results);
            };
            snapshot
                .index
                .matching_page(&needle, local_after, local_work, local_limit)
        };
        let candidates = {
            let snapshot = self.surface_snapshot.borrow();
            let Some(snapshot) = snapshot.as_ref() else {
                return SearchPage::exhausted(revision, results);
            };
            page.ids
                .into_iter()
                .filter_map(|id| snapshot.entries.get(id).cloned().map(|entry| (id, entry)))
                .collect::<Vec<_>>()
        };
        let mut next_after = page.next_after;
        for (id, (surface, action)) in candidates {
            let Some(row) = self.materialize_local_action(&ctx, surface, action) else {
                continue;
            };
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
                next_after = Some(id);
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            results.push(row);
        }
        match next_after {
            Some(next) => Self::live_page(revision, results, "local", next, &needle, &scope),
            None => SearchPage::exhausted(revision, results),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        AuxiliaryEntry, AuxiliaryResume, AuxiliarySnapshot, DIRECTORY_PAGE_REPLY_TIMEOUT,
        DirectoryCacheKey, ID_SEPARATOR, LiveAppActionsProvider, PROVIDER_ID, PendingDirectoryPage,
        SurfaceSnapshot, directory_start_ticket, invalidates_directory_cache,
    };
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::collab_broker::CrdtRoute;
    use crate::shell::collab_broker_capnp::ActionDirectoryBridgeError;
    use crate::shell::collab_broker_capnp::{
        ActionDirectoryRoot, PublishedAction, PublishedInput, PublishedSurface,
    };
    use crate::shell::search::{
        ResourceKind, SearchBudget, SearchContext, SearchProvider, SearchProximity, SearchResult,
    };
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;

    fn context<'a>(
        needle: &'a str,
        chrome: &'a crate::shell::chrome_state::ChromeState,
        doc: &'a AutoCommit,
    ) -> SearchContext<'a> {
        SearchContext {
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
            .workspaces
            .push(Workspace::test_default(WorkspaceId(1), "Search fixture"));
        workspace_tree.active = WorkspaceId(1);
        let tree: WorkspaceTreeHandle = Arc::new(RwLock::new(workspace_tree));
        ChromeState::new_with_handle(tree)
    }

    fn exported(label: &str, action: &str) -> runtime_collab::ExportedAction {
        runtime_collab::ExportedAction {
            resource: format!("resource:{action}"),
            action: action.into(),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            action_label: label.into(),
            description: None,
            operation_kind: "view".into(),
            params_json: "[]".into(),
            contract_json: serde_json::to_string(&tilo_actions::ActionContract::view())
                .expect("view contract JSON"),
            input: runtime_collab::ExportedInput::None,
        }
    }

    fn local_action(label: &str, action: &str) -> PublishedAction {
        PublishedAction {
            resource: format!("resource:{action}"),
            action: action.into(),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            action_label: label.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: tilo_actions::ActionContract::view(),
            input: PublishedInput::None,
        }
    }

    #[test]
    fn directory_lifecycle_invalidates_only_lost_current_authority() {
        for error in [
            ActionDirectoryBridgeError::StaleRevision,
            ActionDirectoryBridgeError::NotFound,
            ActionDirectoryBridgeError::Unavailable,
        ] {
            assert!(invalidates_directory_cache(error));
        }
        for error in [
            ActionDirectoryBridgeError::Busy,
            ActionDirectoryBridgeError::InvalidRequest,
            ActionDirectoryBridgeError::TimedOut,
            ActionDirectoryBridgeError::ConfirmationRequired,
        ] {
            assert!(!invalidates_directory_cache(error));
        }
    }

    #[test]
    fn directory_cache_bounds_rapid_queries_and_expires_stalled_page_and_start_tickets() {
        let provider = LiveAppActionsProvider::new(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let mut prior_sender: Option<
            tokio::sync::oneshot::Sender<
                crate::shell::collab_broker_capnp::ActionDirectoryBridgeReply,
            >,
        > = None;
        for index in 0..10_000_usize {
            let needle = format!("needle-{index}");
            provider.set_directory_query(&needle);
            if let Some(sender) = prior_sender.take() {
                assert!(
                    sender.is_closed(),
                    "changing a query must close the prior palette receiver"
                );
            }
            let key: DirectoryCacheKey = (7, "actor-a".into(), needle, Vec::new());
            provider.directory_pages.borrow_mut().insert(
                key.clone(),
                runtime_collab::ActionDirectoryPage {
                    revision: "r1".into(),
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: true,
                    stale_cursor: false,
                    budget_exhausted: false,
                },
            );
            let (sender, receiver) = tokio::sync::oneshot::channel();
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
        assert!(prior_sender.expect("final sender").is_closed());
        assert!(provider.directory_pages.borrow().is_empty());
        assert!(provider.directory_pending.borrow().is_empty());

        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let (page_sender, page_receiver) = tokio::sync::oneshot::channel();
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
            page_sender.is_closed(),
            "expired directory page ticket is cancelled"
        );
        assert!(provider.directory_pending.borrow().is_empty());

        let invalidations: Rc<RefCell<std::collections::HashSet<DirectoryCacheKey>>> =
            Rc::new(RefCell::new(std::collections::HashSet::default()));
        let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
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
        assert!(
            start_sender.is_closed(),
            "expired action ticket closes its broker reply receiver"
        );
        assert!(invalidations.borrow().contains(&key));
    }

    #[test]
    fn directory_cache_keeps_room_actors_separate_for_the_current_needle() {
        let provider = LiveAppActionsProvider::new(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        provider.set_directory_query("open");
        for actor in ["room-a-user", "room-b-user"] {
            provider.directory_pages.borrow_mut().insert(
                (7, actor.into(), "open".into(), Vec::new()),
                runtime_collab::ActionDirectoryPage {
                    revision: "r1".into(),
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: true,
                    stale_cursor: false,
                    budget_exhausted: false,
                },
            );
        }
        provider.set_directory_query("open");
        assert_eq!(provider.directory_pages.borrow().len(), 2);
        provider.set_directory_query("other");
        assert!(provider.directory_pages.borrow().is_empty());
    }

    #[test]
    fn remote_actions_page_without_repeats_under_tiny_budgets() {
        let provider = LiveAppActionsProvider::new(
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
            app_id: "dev.tilo.fixture".into(),
            cluster_id: "room".into(),
            record_id: "record".into(),
        };
        assert!(
            provider
                .directory_action_result(
                    &root,
                    "r1",
                    &exported("Open one", "one"),
                    "open",
                    "fixture",
                    &[]
                )
                .is_some()
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
            let page = provider.query_page(context("open", &chrome, &doc), cursor.as_ref(), budget);
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

    #[test]
    fn cached_remote_pages_append_after_their_opaque_continuation() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new(broker.clone());
        let chrome = chrome();
        let chrome = chrome.borrow();
        let cluster_id = chrome.tree().workspaces[0].room.to_string();
        let actor = chrome.local_user_for_room(&chrome.tree().workspaces[0].room);
        let root = ActionDirectoryRoot {
            peer_id: 9,
            app_id: "dev.tilo.remote".into(),
            cluster_id,
            record_id: "root".into(),
        };
        broker.replace_action_directory_roots_for_test(vec![root]);
        let row = |label: &str, action: &str| runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource: format!("resource:{action}"),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            resource_presentation_json: String::new(),
            action: Some(action.into()),
            descriptor: Some(exported(label, action)),
            has_children: false,
            visibility_action: None,
            visibility_contract_json: String::new(),
        };
        provider.directory_pages.borrow_mut().insert(
            (9, actor.clone(), "open".into(), Vec::new()),
            runtime_collab::ActionDirectoryPage {
                revision: "r1".into(),
                rows: vec![row("Open first", "first")],
                next_cursor: vec![1],
                exhausted: false,
                stale_cursor: false,
                budget_exhausted: true,
            },
        );
        provider.directory_pages.borrow_mut().insert(
            (9, actor, "open".into(), vec![1]),
            runtime_collab::ActionDirectoryPage {
                revision: "r1".into(),
                rows: vec![row("Open second", "second")],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
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

    #[test]
    fn a_100001_action_snapshot_reaches_a_unique_tail_without_scanning_the_corpus() {
        let provider = LiveAppActionsProvider::new(
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
        );
        let chrome = chrome();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = context("tailtoken", &chrome, &doc);
        let surface = PublishedSurface {
            peer_id: 1,
            app_id: "dev.tilo.fixture".into(),
            route: CrdtRoute::app("dev.tilo.fixture", "room"),
            actions: Vec::new(),
            live: true,
        };
        let entries = (0..100_001)
            .map(|index| {
                let label = if index == 100_000 {
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
        let index = ui::search::SearchTextIndex::new(entries.iter().map(|(surface, action)| {
            format!(
                "{}\u{1f}{}\u{1f}{}",
                action.action_label, action.resource, surface.app_id
            )
        }));
        *provider.surface_snapshot.borrow_mut() = Some(SurfaceSnapshot {
            source_generation: provider.broker.surfaces_generation(),
            scope: LiveAppActionsProvider::surface_scope(&ctx),
            entries,
            index,
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
    fn cold_public_action_page_does_not_start_a_surface_clone_or_index_build() {
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

    #[test]
    fn source_backed_cold_query_does_no_action_index_work_before_refresh() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        broker.replace_published_surfaces_for_test(vec![PublishedSurface {
            peer_id: 1,
            app_id: "dev.tilo.fixture".into(),
            route: CrdtRoute::app("dev.tilo.fixture", "room"),
            actions: (0..100_001)
                .map(|index| {
                    local_action(
                        if index == 100_000 {
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
        let provider = LiveAppActionsProvider::new(broker);
        let chrome = chrome();
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
        // Building the actual 100k broker projection is intentionally real
        // work.  Give a slow shared CI worker a minute to finish rather than
        // turning this cold-path assertion into a scheduler-speed test.
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
    fn auxiliary_cursor_is_self_contained_for_more_than_128_pages() {
        let root = ActionDirectoryRoot {
            peer_id: 42,
            app_id: "dev.tilo.remote".into(),
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

    #[test]
    fn malformed_auxiliary_cursor_fails_closed() {
        assert!(LiveAppActionsProvider::decode_auxiliary_resume("not;a,cursor").is_none());
    }

    #[test]
    fn hierarchy_decodes_the_address_shape_minted_by_the_provider() {
        let id = [
            "dev.tilo.files",
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
                "dev.tilo.files",
                "room-7",
                "file:/Brief.md",
                "files.entry:/Brief.md",
                "rename",
            ))
        );
        assert!(LiveAppActionsProvider::hierarchy("display.id#rename").is_none());
    }

    #[test]
    fn live_action_proximity_uses_the_authenticated_resource_room() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new(broker);
        let chrome = chrome();
        let chrome = chrome.borrow();
        let room = chrome.tree().workspaces[0].room.to_string();
        let result = |cluster: &str| SearchResult {
            kind: ResourceKind::Action,
            provider_id: PROVIDER_ID.into(),
            id: [
                "dev.tilo.files",
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the authenticated directory traversal scenario is clearest as one test"
    )]
    fn resource_only_directory_row_enters_an_authenticated_child_scope() {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        let provider = LiveAppActionsProvider::new(broker.clone());
        let chrome = chrome();
        let cluster_id = chrome.borrow().tree().workspaces[0].room.to_string();
        let actor = chrome
            .borrow()
            .local_user_for_room(&chrome.borrow().tree().workspaces[0].room);
        let root = ActionDirectoryRoot {
            peer_id: 9,
            app_id: "dev.tilo.reader".into(),
            cluster_id: cluster_id.clone(),
            record_id: "library".into(),
        };
        broker.replace_action_directory_roots_for_test(vec![root]);
        provider.directory_pages.borrow_mut().insert(
            (9, actor, "book".into(), Vec::new()),
            runtime_collab::ActionDirectoryPage {
                revision: "r1".into(),
                rows: vec![runtime_collab::ActionDirectoryRow {
                    parent_resource: String::new(),
                    resource: "book:42".into(),
                    resource_kind: "book".into(),
                    resource_label: "Book of Tilo".into(),
                    resource_presentation_json: serde_json::to_string(
                        &tilo_actions::ResourcePresentation {
                            detail: Some("Chapter 7".into()),
                            badge: Some("2 collaborators".into()),
                            preview: None,
                        },
                    )
                    .unwrap(),
                    action: None,
                    descriptor: None,
                    has_children: true,
                    visibility_action: None,
                    visibility_contract_json: String::new(),
                }],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
        );
        let doc = AutoCommit::new();
        let wanted = [ResourceKind::Custom("Resources".into())];
        let chrome_ref = chrome.borrow();
        let page = provider.query_page(
            SearchContext {
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
            Some("dev.tilo.reader · Chapter 7")
        );
        assert_eq!(page.results[0].badge.as_deref(), Some("2 collaborators"));
        (page.results[0].on_open)(
            &mut AutoCommit::new(),
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
                app_id: "dev.tilo.reader".into(),
                resource: "book:42".into(),
                label: "Book of Tilo".into(),
            })
        );
        let chrome_ref = chrome.borrow();
        let _ = provider.query_page(
            SearchContext {
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
                .map(|query| query.parent_resource.as_str()),
            Some("book:42"),
            "the child fetch must carry the selected stable resource address"
        );
    }
}
