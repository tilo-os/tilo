//! Dynamic provider backed by the resource/action surfaces live qOS apps
//! publish through the session collaboration broker.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::ActionReceipt;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{
    InvocationDisposition, PendingInvocation, ResourceKind, SearchContext, SearchProvider,
    SearchResult,
};

use crate::shell::collab_broker_capnp::{
    ActionDirectoryBridgeResponse, ActionDirectoryRequestQueue, BrokerActor,
    DirectoryActionConfirmation, PublishedAction, PublishedInput, PublishedSurface,
};

pub const PROVIDER_ID: &str = "app-actions";
const ID_SEPARATOR: char = '\u{1f}';
const MAX_DIRECTORY_ROOTS_PER_QUERY: usize = 16;
/// A stalled LocalSet/capability must not leave a palette-owned oneshot alive
/// forever. Retrying after this deadline remains bounded by the bridge queue.
const DIRECTORY_PAGE_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

type DirectoryCacheKey = (u64, String, String);

struct PendingDirectoryPage {
    receiver: tokio::sync::oneshot::Receiver<
        crate::shell::collab_broker_capnp::ActionDirectoryBridgeReply,
    >,
    deadline: Instant,
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
    directory_invalidations: Rc<RefCell<HashSet<(u64, String, String)>>>,
    directory_epoch: AtomicU64,
}

struct PeerInvocation<'a> {
    peer_id: u64,
    resource: &'a str,
    action: &'a str,
    argument: Option<&'a str>,
    actor: &'a str,
    confirmation: &'a qos_actions::ConfirmationPolicy,
    confirmed: bool,
    phrase: Option<&'a str>,
}

fn start_on_peer(
    broker: &AppActionBroker,
    invocation: &PeerInvocation<'_>,
) -> Result<crate::shell::collab_broker_capnp::InvocationJob, String> {
    match invocation.confirmation {
        qos_actions::ConfirmationPolicy::None => broker.start(
            invocation.peer_id,
            invocation.resource,
            invocation.action,
            invocation.argument,
            invocation.actor,
        ),
        qos_actions::ConfirmationPolicy::Confirm => {
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
        qos_actions::ConfirmationPolicy::Phrase { .. } => {
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
    let update = broker
        .status(job.id)
        .unwrap_or_else(|| qos_actions::InvocationUpdate::Accepted {
            invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job.id))),
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
            qos_actions::ARGUMENTS_JSON_PREFIX,
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
    pub fn new(broker: AppActionBroker) -> Self {
        let directory_queue = broker.action_directory_request_queue();
        Self {
            broker,
            directory_queue,
            directory_pages: RefCell::new(HashMap::new()),
            directory_pending: RefCell::new(HashMap::new()),
            directory_query: RefCell::new(None),
            directory_invalidations: Rc::new(RefCell::new(HashSet::new())),
            directory_epoch: AtomicU64::new(0),
        }
    }

    fn poll_directory_pages_at(&self, now: Instant) {
        let mut ready = Vec::new();
        self.directory_pending.borrow_mut().retain(|key, pending| {
            if pending.deadline <= now {
                return false;
            }
            match pending.receiver.try_recv() {
                Ok(Ok(ActionDirectoryBridgeResponse::Page(page))) if !page.stale_cursor => {
                    ready.push((key.clone(), page));
                    false
                }
                Ok(Err(_)) | Ok(Ok(_)) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    false
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true,
            }
        });
        if !ready.is_empty() {
            let mut pages = self.directory_pages.borrow_mut();
            for (key, page) in ready {
                pages.insert(key, page);
            }
            self.directory_epoch.fetch_add(1, Ordering::Relaxed);
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
            .retain(|(_, _, cached_needle), _| cached_needle == &next);
        self.directory_pending
            .borrow_mut()
            .retain(|(_, _, cached_needle), _| cached_needle == &next);
        self.directory_epoch.fetch_add(1, Ordering::Relaxed);
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

    fn request_directory_page(&self, peer_id: u64, cluster_id: &str, actor: &str, needle: &str) {
        let key = (peer_id, actor.to_string(), needle.to_string());
        if self.directory_pages.borrow().contains_key(&key)
            || self.directory_pending.borrow().contains_key(&key)
        {
            return;
        }
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: String::new(),
            cursor: Vec::new(),
            needle: needle.to_string(),
            parent_resource: String::new(),
            max_results: 48,
            max_bytes: 48 * 1024,
            max_work: 4_096,
            max_depth: 1,
        };
        if let Ok(receiver) =
            self.directory_queue
                .try_query(BrokerActor::person(actor), cluster_id, query)
        {
            self.directory_pending.borrow_mut().insert(
                key,
                PendingDirectoryPage {
                    receiver,
                    deadline: deadline_after(DIRECTORY_PAGE_REPLY_TIMEOUT),
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

    pub fn capability_target(id: &str) -> Option<(&str, qos_actions::PermissionRequirement, &str)> {
        let (app, permission, _cluster, _record, _resource, action) = Self::result_parts(id)?;
        let permission = match permission {
            "none" => qos_actions::PermissionRequirement::None,
            "read" => qos_actions::PermissionRequirement::Read,
            "comment" => qos_actions::PermissionRequirement::Comment,
            "write" => qos_actions::PermissionRequirement::Write,
            "invite" => qos_actions::PermissionRequirement::Invite,
            "admin" => qos_actions::PermissionRequirement::Admin,
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
                qos_actions::PermissionRequirement::None => "none",
                qos_actions::PermissionRequirement::Read => "read",
                qos_actions::PermissionRequirement::Comment => "comment",
                qos_actions::PermissionRequirement::Write => "write",
                qos_actions::PermissionRequirement::Invite => "invite",
                qos_actions::PermissionRequirement::Admin => "admin",
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

    fn matches_scope(surface: &PublishedSurface, ctx: &SearchContext<'_>) -> bool {
        let active_room = ctx
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        match ctx.chrome.search_scope {
            crate::shell::chrome_state::SearchScope::All => true,
            crate::shell::chrome_state::SearchScope::Workspace => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
            }
            crate::shell::chrome_state::SearchScope::ActiveApp => {
                active_room.as_deref() == Some(surface.route.cluster_id.as_str())
                    && ctx
                        .chrome
                        .active_app_id()
                        .is_some_and(|app| app.eq_ignore_ascii_case(&surface.app_id))
            }
        }
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
            qos_actions::ConfirmationPolicy::None => {}
            qos_actions::ConfirmationPolicy::Confirm => params.push(ActionParam::required(
                "confirm",
                ParamType::Boolean,
                "Explicitly confirm this consequential action",
            )),
            qos_actions::ConfirmationPolicy::Phrase { .. } => {
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
            serde_json::from_str::<qos_actions::ActionContract>(&descriptor.contract_json).ok()?;
        let mut params = serde_json::from_str::<Vec<ActionParam>>(&descriptor.params_json).ok()?;
        match &contract.confirmation {
            qos_actions::ConfirmationPolicy::None => {}
            qos_actions::ConfirmationPolicy::Confirm => params.push(ActionParam::required(
                "confirm",
                ParamType::Boolean,
                "Explicitly confirm this consequential action",
            )),
            qos_actions::ConfirmationPolicy::Phrase { .. } => params.push(ActionParam::required(
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
        let cache_key = (peer_id, actor.clone(), needle.to_string());
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
                        qos_actions::ARGUMENTS_JSON_PREFIX,
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
}

impl SearchProvider for LiveAppActionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Application actions"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
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

    #[allow(
        clippy::too_many_lines,
        reason = "the query constructs paired palette/headless callbacks from one captured action contract; splitting them would duplicate or desynchronise confirmation, launch, and job lifecycle state"
    )]
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let needle = ctx.needle.trim().to_lowercase();
        let actor = ctx.chrome.active_local_user();
        let mut results = Vec::new();
        self.set_directory_query(&needle);
        self.apply_directory_invalidations();
        self.poll_directory_pages();
        for root in self
            .broker
            .action_directory_root_page(None, MAX_DIRECTORY_ROOTS_PER_QUERY)
            .roots
        {
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
            self.request_directory_page(root.peer_id, &root.cluster_id, &root_actor, &needle);
            let page = self
                .directory_pages
                .borrow()
                .get(&(root.peer_id, root_actor.clone(), needle.clone()))
                .cloned();
            if let Some(page) = page {
                for row in page.rows {
                    if let Some(descriptor) = row.descriptor {
                        if let Some(result) = self.directory_action_result(
                            &root,
                            &page.revision,
                            &descriptor,
                            &needle,
                            &root_actor,
                        ) {
                            results.push(result);
                        }
                    }
                }
            }
        }
        if needle.is_empty() || "jobs status progress".contains(&needle) {
            for (id, update) in self.broker.statuses(&actor) {
                let state = match &update {
                    qos_actions::InvocationUpdate::Accepted { .. } => "accepted",
                    qos_actions::InvocationUpdate::Progress { .. } => "running",
                    qos_actions::InvocationUpdate::Item { .. } => "streaming",
                    qos_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
                    qos_actions::InvocationUpdate::Failed { .. } => "failed",
                    qos_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
                };
                let status_broker = self.broker.clone();
                results.push(SearchResult {
                    kind: ResourceKind::Custom("action-job".into()),
                    provider_id: PROVIDER_ID.into(),
                    id: job_status_id(id),
                    title: format!("Action job {id}"),
                    subtitle: Some(state.into()),
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
        for surface in self.broker.surfaces() {
            if !Self::matches_scope(&surface, &ctx) {
                continue;
            }
            for action in &surface.actions {
                let unavailable_reason = match &action.contract.availability {
                    qos_actions::Availability::Available => None,
                    qos_actions::Availability::Unavailable { reason, .. } => Some(reason.clone()),
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
                                qos_actions::ARGUMENTS_JSON_PREFIX,
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
                                qos_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                qos_actions::ConfirmationPolicy::Phrase { phrase: expected }
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
                                .name("qos-deferred-action".into())
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
                                qos_actions::ConfirmationPolicy::Confirm if !confirmed => {
                                    return Err(ActionError::Refused(
                                        "explicit confirmation is required".into(),
                                    ));
                                }
                                qos_actions::ConfirmationPolicy::Phrase { phrase: expected }
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
        // The provider itself also caps its merged output: the legacy
        // trait adapter is a second guard, not permission to materialize an
        // unbounded number of per-app directory rows first.
        results.truncate(48);
        results
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.directory_epoch.load(Ordering::Relaxed).to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::{
        DIRECTORY_PAGE_REPLY_TIMEOUT, DirectoryCacheKey, ID_SEPARATOR, LiveAppActionsProvider,
        PendingDirectoryPage, directory_start_ticket, invalidates_directory_cache,
    };
    use crate::shell::collab_broker_capnp::ActionDirectoryBridgeError;

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
            let key: DirectoryCacheKey = (7, "actor-a".into(), needle);
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
            (7, "actor-a".into(), "new-needle".into()),
            PendingDirectoryPage {
                receiver: page_receiver,
                deadline: expired,
            },
        );
        provider.poll_directory_pages_at(Instant::now());
        assert!(
            page_sender.is_closed(),
            "expired directory page ticket is cancelled"
        );
        assert!(provider.directory_pending.borrow().is_empty());

        let invalidations: Rc<RefCell<std::collections::HashSet<DirectoryCacheKey>>> =
            Rc::new(RefCell::new(Default::default()));
        let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
        let key = (7, "actor-a".into(), "new-needle".into());
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
                (7, actor.into(), "open".into()),
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
    fn hierarchy_decodes_the_address_shape_minted_by_the_provider() {
        let id = [
            "dev.qos.files",
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
                "dev.qos.files",
                "room-7",
                "file:/Brief.md",
                "files.entry:/Brief.md",
                "rename",
            ))
        );
        assert!(LiveAppActionsProvider::hierarchy("display.id#rename").is_none());
    }
}
