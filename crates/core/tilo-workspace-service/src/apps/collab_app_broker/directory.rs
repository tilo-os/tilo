#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// A live application's pull-only directory.  Keeping the certified route
/// beside the capability makes every page/resolve check independent of a
/// stale eager `publishActions` snapshot.
#[derive(Clone)]
pub(super) struct RegisteredActionDirectory {
    pub(super) pagination: directory_pagination::DirectoryPagination,
    /// Certified publishing person, never a presence label or workspace role.
    pub(super) publishing_person: Option<String>,
    pub(super) app_id: String,
    pub(super) route: CrdtRoute,
    pub(super) directory: Rc<dyn wire::DirectorySource>,
    pub(super) registration: Rc<uuid::Uuid>,
}

/// Inert, enumerable route metadata for one connected pull directory.  It
/// contains no capability and disappears on disconnect, so shell search can
/// select a source without reaching into the broker `LocalSet`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the four identifiers name four different values; the shared suffix says which kind each is"
)]
pub struct ActionDirectoryRoot {
    pub peer_id: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub document_id: String,
}

/// A deterministic, inert page over the broker-retained directory root
/// projection.  Rebuilding the projection is a connect/disconnect operation;
/// palette keystrokes only clone the requested small page.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActionDirectoryRootPage {
    pub roots: Vec<ActionDirectoryRoot>,
    pub next_after_peer: Option<u64>,
    pub generation: u64,
}

#[derive(Default)]
pub(super) struct ActionDirectoryRootSnapshot {
    pub(super) roots: Vec<ActionDirectoryRoot>,
    pub(super) generation: u64,
}

/// `LocalSet`-only ownership for Rc-backed application capabilities.  The
/// shared broker state deliberately stores neither this map nor its receiver:
/// shell/chrome worker threads must remain Send.
pub(super) struct LocalActionDirectoryRegistry {
    pub(super) directories: RefCell<HashMap<u64, RegisteredActionDirectory>>,
    pub(super) bridge_rx: RefCell<std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>>,
    pub(super) resolved_permits: Arc<tokio::sync::Semaphore>,
}

impl LocalActionDirectoryRegistry {
    pub(super) fn new(bridge_rx: std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>) -> Self {
        Self {
            directories: RefCell::new(HashMap::new()),
            bridge_rx: RefCell::new(bridge_rx),
            resolved_permits: Arc::new(tokio::sync::Semaphore::new(64)),
        }
    }

    pub(super) fn registered(
        &self,
        peer_id: u64,
        cluster_id: &str,
    ) -> Result<RegisteredActionDirectory, BrokerError> {
        self.directories
            .borrow()
            .get(&peer_id)
            .filter(|entry| entry.route.cluster_id == cluster_id)
            .cloned()
            .ok_or_else(|| BrokerError::failed("unknown live application directory".into()))
    }

    pub(super) fn remove(&self, peer_id: u64) {
        // Dropping the removed directory handle can synchronously release a
        // connection lease and re-enter this idempotent cleanup. Release the
        // registry borrow before capability drop so the nested removal sees
        // an ordinary absent entry instead of panicking on RefCell reborrow.
        let removed = self.directories.borrow_mut().remove(&peer_id);
        drop(removed);
    }
}

/// Inert result returned from the broker `LocalSet` to shell chrome.  It never
/// carries a broker handle or widget reference across threads.
#[derive(Debug)]
pub enum ActionDirectoryBridgeResponse {
    Page(runtime_collab::ActionDirectoryPage),
    Resolution(runtime_collab::ActionDirectoryResolution),
}

/// A bounded bridge failure.  `Busy` is deliberate backpressure: palette
/// code must retain its last cached results rather than blocking a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDirectoryBridgeError {
    Busy,
    InvalidRequest,
    Unavailable,
    TimedOut,
    StaleRevision,
    NotFound,
    ConfirmationRequired,
}

pub type ActionDirectoryBridgeReply =
    Result<ActionDirectoryBridgeResponse, ActionDirectoryBridgeError>;

/// Where a directory bridge answer goes: a oneshot any executor may await,
/// or a remote-action [`tilo_actions::remote::Pending`] completed on the
/// broker `LocalSet` in the same turn the owner answered.
pub(super) enum DirectoryReply {
    Oneshot(tokio::sync::oneshot::Sender<ActionDirectoryBridgeReply>),
    Pending(tilo_actions::remote::Completer<ActionDirectoryBridgeReply>),
}

impl DirectoryReply {
    /// The longest this asker is waited for. The shell's own surfaces ask
    /// for a person mid-frame and keep their last results past a short
    /// deadline. A remote caller has a deadline of its own and gives up by
    /// dropping its answer, which ends the wait sooner; the bound here only
    /// stops an owner that never answers from pinning the request.
    pub(super) fn patience(&self) -> Duration {
        match self {
            Self::Oneshot(_) => AppBrokerState::DIRECTORY_BRIDGE_TIMEOUT,
            Self::Pending(_) => AppBrokerState::DIRECTORY_OWNER_HUNG,
        }
    }

    /// `answer`, awaited for as long as this asker still wants it: the
    /// owning app answers on its next turn, and a turn running a heavy
    /// action or frame is busy, not gone. `Err(TimedOut)` once the asker's
    /// patience ran out, `Err(Unavailable)` once it stopped asking.
    pub(super) async fn awaited<T>(
        &mut self,
        answer: impl std::future::Future<Output = T>,
    ) -> Result<T, ActionDirectoryBridgeError> {
        let patience = self.patience();
        match self {
            Self::Oneshot(sender) => await_while_asked(patience, sender.closed(), answer).await,
            Self::Pending(completer) => {
                await_while_asked(patience, completer.abandoned(), answer).await
            }
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        match self {
            Self::Oneshot(sender) => sender.is_closed(),
            Self::Pending(completer) => completer.is_abandoned(),
        }
    }

    pub(super) fn send(self, reply: ActionDirectoryBridgeReply) {
        match self {
            Self::Oneshot(sender) => {
                let _ = sender.send(reply);
            }
            Self::Pending(completer) => completer.complete(reply),
        }
    }
}

/// `answer`, or why it stopped being awaited: `patience` ran out
/// (`TimedOut`) or the asker went away — `gone` resolved — (`Unavailable`).
/// The asker's departure is an event, so a cancelled request is released
/// at once rather than on the next check.
pub(super) async fn await_while_asked<T>(
    patience: Duration,
    gone: impl std::future::Future<Output = ()>,
    answer: impl std::future::Future<Output = T>,
) -> Result<T, ActionDirectoryBridgeError> {
    tokio::select! {
        biased;
        value = answer => Ok(value),
        () = gone => Err(ActionDirectoryBridgeError::Unavailable),
        () = tokio::time::sleep(patience) => Err(ActionDirectoryBridgeError::TimedOut),
    }
}

#[cfg(test)]
mod directory_patience_tests {
    use super::super::{AppBrokerState, Duration};
    use super::{ActionDirectoryBridgeError, DirectoryReply, await_while_asked};

    /// A remote caller (an agent, another device) waits for an app whose
    /// owner thread is busy with a heavy action or frame for longer than the
    /// shell's own surfaces do; how long is the caller's to say.
    #[test]
    pub(super) fn a_remote_asker_outwaits_a_busy_owner_and_stops_when_it_gives_up() {
        let (completer, answer) =
            tilo_actions::remote::pending::<super::ActionDirectoryBridgeReply>();
        let remote = DirectoryReply::Pending(tilo_actions::remote::pending().0);
        let (shell, _receiver) = tokio::sync::oneshot::channel();
        let shell = DirectoryReply::Oneshot(shell);
        assert!(remote.patience() > shell.patience());
        assert!(
            AppBrokerState::DIRECTORY_OWNER_HUNG > AppBrokerState::DIRECTORY_BRIDGE_TIMEOUT * 10
        );
        drop(remote);

        // Paused: the owner's busy spell and the patience are timed by the
        // test's clock, never by how promptly a loaded host runs the timers.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let busy = Duration::from_millis(300);
            let answered =
                await_while_asked(Duration::from_secs(5), std::future::pending(), async {
                    tokio::time::sleep(busy).await;
                    7
                })
                .await;
            assert_eq!(answered, Ok(7), "a busy owner is waited for");

            let hung = await_while_asked(
                Duration::from_millis(200),
                std::future::pending(),
                std::future::pending::<()>(),
            )
            .await;
            assert_eq!(hung, Err(ActionDirectoryBridgeError::TimedOut));

            let mut reply = DirectoryReply::Pending(completer);
            drop(answer);
            let abandoned = tilo_test_env::within(
                "an abandoned asker stops the wait",
                reply.awaited(std::future::pending::<()>()),
            )
            .await;
            assert_eq!(abandoned, Err(ActionDirectoryBridgeError::Unavailable));
        });
    }

    /// An asker that goes away — a shell surface dropping its oneshot, a
    /// remote caller dropping its answer — releases the directory request
    /// the moment it does. The clock is paused: any timer the wait armed
    /// to notice would have to advance it, and none may.
    #[tokio::test(start_paused = true)]
    pub(super) async fn directory_wait_ends_immediately_when_asker_drops() {
        let (shell, receiver) = tokio::sync::oneshot::channel();
        let (completer, answer) =
            tilo_actions::remote::pending::<super::ActionDirectoryBridgeReply>();
        for (mut reply, asker) in [
            (
                DirectoryReply::Oneshot(shell),
                Box::new(receiver) as Box<dyn Send>,
            ),
            (DirectoryReply::Pending(completer), Box::new(answer)),
        ] {
            let started = tokio::time::Instant::now();
            let owner_never_answers = reply.awaited(std::future::pending::<()>());
            tokio::pin!(owner_never_answers);
            assert!(
                futures::poll!(owner_never_answers.as_mut()).is_pending(),
                "the asker is still there"
            );
            drop(asker);
            assert_eq!(
                owner_never_answers.await,
                Err(ActionDirectoryBridgeError::Unavailable)
            );
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "the departure woke the wait; no timer had to notice it"
            );
        }
    }
}

pub(super) struct EagerActionInvokeRequest {
    pub(super) caller: tilo_actions::remote::Caller,
    pub(super) own_peer: Option<u64>,
    pub(super) launch: Option<LaunchScope>,
    pub(super) origin: InvocationCaller,
    pub(super) request: tilo_actions::remote::InvokeRequest,
    pub(super) reply: tilo_actions::remote::Completer<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    >,
}

pub(super) enum ActionDirectoryBridgeRequest {
    InvokeEager(Box<EagerActionInvokeRequest>),
    Query {
        actor: BrokerActor,
        cluster_id: String,
        query: runtime_collab::ActionDirectoryQuery,
        reply: DirectoryReply,
    },
    Resolve {
        actor: BrokerActor,
        cluster_id: String,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        reply: DirectoryReply,
    },
}

/// Thread-safe sender for palette/search code.  The receiver is drained only
/// by the broker's `LocalSet`, where its Rc-backed capability clients remain
/// valid.  Callers receive a oneshot they may poll from any executor.
#[derive(Clone)]
pub struct ActionDirectoryRequestQueue {
    pub(super) sender: std::sync::mpsc::SyncSender<ActionDirectoryBridgeRequest>,
    /// Wakes the accept loop, which is the only drainer of this queue.
    pub(super) turn: std::sync::Arc<tokio::sync::Notify>,
}

impl ActionDirectoryRequestQueue {
    pub(super) const CAPACITY: usize = 64;

    pub(crate) fn unavailable() -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        drop(receiver);
        Self {
            sender,
            turn: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(super) fn deliver(
        &self,
        request: ActionDirectoryBridgeRequest,
    ) -> Result<(), ActionDirectoryBridgeError> {
        match self.sender.try_send(request) {
            Ok(()) => {
                self.turn.notify_one();
                Ok(())
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(ActionDirectoryBridgeError::Busy),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ActionDirectoryBridgeError::Unavailable)
            }
        }
    }

    pub fn try_query(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        let reply = DirectoryReply::Oneshot(reply);
        self.deliver(ActionDirectoryBridgeRequest::Query {
            actor,
            cluster_id: cluster_id.into(),
            query,
            reply,
        })
        .map(|()| response)
    }

    pub fn try_resolve(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        let reply = DirectoryReply::Oneshot(reply);
        self.deliver(ActionDirectoryBridgeRequest::Resolve {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            reply,
        })
        .map(|()| response)
    }

    /// [`Self::try_query`] answered as a remote-action pending value.
    pub(crate) fn try_query_pending(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
        self.enqueue(ActionDirectoryBridgeRequest::Query {
            actor,
            cluster_id: cluster_id.into(),
            query,
            reply: DirectoryReply::Pending(reply),
        })
        .map(|()| response)
    }

    /// [`Self::try_resolve`] answered as a remote-action pending value.
    pub(crate) fn try_resolve_pending(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
        self.enqueue(ActionDirectoryBridgeRequest::Resolve {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            reply: DirectoryReply::Pending(reply),
        })
        .map(|()| response)
    }

    pub(super) fn enqueue(
        &self,
        request: ActionDirectoryBridgeRequest,
    ) -> Result<(), ActionDirectoryBridgeError> {
        self.deliver(request)
    }
}

pub(super) const DIRECTORY_MAX_IDENTIFIER_BYTES: usize = 512;

pub(super) const DIRECTORY_MAX_KIND_BYTES: usize = 256;

pub(super) const DIRECTORY_MAX_LABEL_BYTES: usize = 4 * 1024;

pub(super) const DIRECTORY_MAX_DESCRIPTION_BYTES: usize = 8 * 1024;

// A directory row's contract, parameter declarations and presentation are
// bounded where they are decoded (`runtime_collab::MAX_DIRECTORY_*_WIRE_BYTES`);
// this bounds the argument an invocation carries.
pub(super) const DIRECTORY_MAX_PARAMS_BYTES: usize = 32 * 1024;

pub(super) const DIRECTORY_MAX_QUERY_REVISION_BYTES: usize = 512;

pub(super) const DIRECTORY_MAX_QUERY_NEEDLE_BYTES: usize = 4 * 1024;

pub(super) const DIRECTORY_MAX_CURSOR_BYTES: usize = 4 * 1024;

pub(super) const DIRECTORY_MAX_CONFIRMATION_BYTES: usize = 4 * 1024;

pub(super) fn validate_directory_text(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), BrokerError> {
    if (required && value.is_empty()) || value.len() > maximum || value.contains('\0') {
        return Err(BrokerError::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

pub(super) fn validate_directory_identifier(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), BrokerError> {
    validate_directory_text(value, field, maximum, required)?;
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(BrokerError::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

pub(super) fn validate_directory_query_input(
    query: &runtime_collab::ActionDirectoryQuery,
) -> Result<(), BrokerError> {
    validate_directory_text(
        &query.revision,
        "query revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_text(
        &query.needle,
        "query needle",
        DIRECTORY_MAX_QUERY_NEEDLE_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &query.parent_resource,
        "query parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    if query.cursor.len() > DIRECTORY_MAX_CURSOR_BYTES {
        return Err(BrokerError::failed(
            "invalid action directory query cursor".into(),
        ));
    }
    validate_directory_text(
        &query.resource_name,
        "query resource name",
        DIRECTORY_MAX_LABEL_BYTES,
        false,
    )?;
    // A name lookup is one whole answer about one level (the top, or one
    // parent's children): it neither continues a page nor narrows a needle.
    if !query.resource_name.is_empty() && (!query.cursor.is_empty() || !query.needle.is_empty()) {
        return Err(BrokerError::failed(
            "a resource name lookup takes no cursor or needle".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod directory_query_input_tests {
    use super::validate_directory_query_input;

    pub(super) fn named(parent: &str) -> runtime_collab::ActionDirectoryQuery {
        runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: Vec::new(),
            needle: String::new(),
            parent_resource: parent.into(),
            max_results: 8,
            max_bytes: 65536,
            max_work: 1,
            max_depth: 1,
            resource_name: "Todo".into(),
        }
    }

    /// A name is asked at the top level or among one parent's children,
    /// whole: never continued by a cursor or narrowed by a needle.
    #[test]
    pub(super) fn a_name_is_asked_at_one_level_whole() {
        assert!(validate_directory_query_input(&named("")).is_ok());
        assert!(validate_directory_query_input(&named("notes.notebook")).is_ok());
        let mut continued = named("notes.notebook");
        continued.cursor = b"next".to_vec();
        assert!(validate_directory_query_input(&continued).is_err());
        let mut narrowed = named("");
        narrowed.needle = "milk".into();
        assert!(validate_directory_query_input(&narrowed).is_err());
    }
}

pub(super) fn validate_directory_target_input(
    cluster_id: &str,
    revision: &str,
    resource: &str,
    action: &str,
    argument: Option<&str>,
) -> Result<(), BrokerError> {
    validate_directory_identifier(
        cluster_id,
        "directory cluster",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_text(
        revision,
        "directory revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_identifier(
        resource,
        "directory resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        action,
        "directory action",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    if let Some(argument) = argument {
        validate_directory_text(
            argument,
            "directory argument",
            DIRECTORY_MAX_PARAMS_BYTES,
            false,
        )?;
    }
    Ok(())
}

/// Validate every untrusted field before it reaches shell chrome, then
/// apply current authority. `Ok(false)` means a structurally valid row is
/// simply not visible to this actor; it must not suppress permitted
/// siblings on the same source page.
pub(super) fn permits_directory_row(
    state: &AppBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    publishing_person: Option<&str>,
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<bool, BrokerError> {
    let question = directory_row_question(row)?;
    if !question.disclosed_to(actor, publishing_person) {
        return Ok(false);
    }
    Ok(state
        .permits_directory_actions(cluster_id, actor, app_id, &[question])?
        .first()
        .is_some_and(|answer| *answer != Answer::Deny))
}

/// What current authority must decide for one directory row: the action
/// whose contract governs the row, where it sits and what kind it is.
pub(super) struct DirectoryQuestion<'a> {
    pub(super) resource_kind: &'a str,
    pub(super) resource: &'a str,
    pub(super) action: &'a str,
    pub(super) field: Option<&'a str>,
    pub(super) kind: ActionKind,
    pub(super) private_view: bool,
    pub(super) shown_here: bool,
}

impl<'a> DirectoryQuestion<'a> {
    /// The question the row's decoded contract asks. Resource-only rows
    /// carry the visibility contract.
    pub(super) fn of(
        resource_kind: &'a str,
        resource: &'a str,
        action: &'a str,
        contract: &'a tilo_actions::ActionContract,
        operation: Option<tilo_actions::OperationKind>,
    ) -> Self {
        Self {
            resource_kind,
            resource,
            action,
            field: contract.field.as_deref(),
            kind: contract.need().kind,
            // Read kind alone does not make a private UI view public.
            // A declared document read, however, discloses document state
            // under the caller's ordinary resource policy, not owner UI.
            // That exception requires a known View kind: a tag alone cannot
            // disclose resource-only metadata with an unknown governing kind.
            // Another read of the record (`review-restore`) discloses it the
            // same way. A control the window draws is its person's pointer
            // and keyboard: whatever it offers is theirs alone.
            private_view: tilo_actions::kind_is_a_drawn_control(resource_kind)
                || ((operation == Some(tilo_actions::OperationKind::View) || operation.is_none())
                    && !contract.is_presence_view()
                    && !(operation == Some(tilo_actions::OperationKind::View)
                        && contract.reads_resource())),
            // An effect that only opens a form or shows a place on its
            // publisher's screen is theirs alone, as a view is.
            shown_here: operation == Some(tilo_actions::OperationKind::Effect)
                && contract.only_shows(),
        }
    }

    pub(super) fn disclosed_to(
        &self,
        actor: &BrokerActor,
        publishing_person: Option<&str>,
    ) -> bool {
        !(self.private_view || self.shown_here)
            || actor
                .actor()
                .person_id()
                .is_some_and(|me| publishing_person == Some(me))
    }

    /// Whether an agent that names this row's action is told it is its
    /// publisher's alone ([`tilo_actions::Availability::shown_here`]),
    /// though it is never listed to it: an effect that only changes what the
    /// publisher's screen shows, not a control or a private view.
    pub(super) fn refused_by_name_to(&self, actor: &BrokerActor) -> bool {
        self.shown_here && !self.private_view && actor.is_agent()
    }
}

/// Validate every capability-bearing field of one untrusted directory row
/// and name the policy question it asks. No authority is read here.
#[allow(
    clippy::too_many_lines,
    reason = "directory row authorization validates every capability-bearing field before the result reaches shell search"
)]
pub(super) fn directory_row_question(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<DirectoryQuestion<'_>, BrokerError> {
    validate_directory_identifier(
        &row.parent_resource,
        "parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &row.resource,
        "resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        &row.resource_kind,
        "resource kind",
        DIRECTORY_MAX_KIND_BYTES,
        true,
    )?;
    validate_directory_text(
        &row.resource_label,
        "resource label",
        DIRECTORY_MAX_LABEL_BYTES,
        true,
    )?;
    match (&row.action, &row.descriptor) {
        (Some(action), Some(descriptor)) => {
            validate_directory_identifier(action, "action", DIRECTORY_MAX_IDENTIFIER_BYTES, true)?;
            if descriptor.resource != row.resource
                || descriptor.action != *action
                || descriptor.resource_kind != row.resource_kind
                || descriptor.resource_label != row.resource_label
                || row.has_children
                || row.visibility.as_ref().is_none_or(|visibility| {
                    visibility.action != *action
                        || visibility.contract != descriptor.contract
                        || visibility
                            .governing_kind
                            .is_some_and(|kind| kind != descriptor.operation_kind)
                })
            {
                return Err(BrokerError::failed(
                    "directory action row does not match its descriptor".into(),
                ));
            }
            validate_directory_text(
                &descriptor.action_label,
                "action label",
                DIRECTORY_MAX_LABEL_BYTES,
                true,
            )?;
            if descriptor
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.len() > DIRECTORY_MAX_DESCRIPTION_BYTES
                        || description.contains('\0')
                })
            {
                return Err(BrokerError::failed(
                    "invalid action directory description".into(),
                ));
            }
            match &descriptor.input {
                runtime_collab::ExportedInput::None => {}
                runtime_collab::ExportedInput::Text {
                    prompt,
                    placeholder,
                    ..
                } => {
                    validate_directory_text(
                        prompt,
                        "input prompt",
                        DIRECTORY_MAX_LABEL_BYTES,
                        true,
                    )?;
                    validate_directory_text(
                        placeholder,
                        "input placeholder",
                        DIRECTORY_MAX_LABEL_BYTES,
                        false,
                    )?;
                }
            }
            let published = published_directory_action(row)?;
            validate_resource_view_descriptors(std::slice::from_ref(&published))
                .map_err(BrokerError::failed)?;
            Ok(DirectoryQuestion::of(
                &row.resource_kind,
                &row.resource,
                action,
                &descriptor.contract,
                Some(descriptor.operation_kind),
            ))
        }
        (None, None) => {
            let Some(visibility) = &row.visibility else {
                return Err(BrokerError::failed(
                    "directory resource row has no visibility contract".into(),
                ));
            };
            validate_directory_identifier(
                &visibility.action,
                "visibility action",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
            )?;
            Ok(DirectoryQuestion::of(
                &row.resource_kind,
                &row.resource,
                &visibility.action,
                &visibility.contract,
                visibility.governing_kind,
            ))
        }
        _ => Err(BrokerError::failed(
            "directory row mixes resource and action shapes".into(),
        )),
    }
}

pub(super) fn filter_directory_page(
    state: &AppBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    publishing_person: Option<&str>,
    mut page: runtime_collab::ActionDirectoryPage,
    max_results: u32,
) -> Result<runtime_collab::ActionDirectoryPage, BrokerError> {
    validate_directory_text(
        &page.revision,
        "directory revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    if page.next_cursor.len() > DIRECTORY_MAX_CURSOR_BYTES {
        return Err(BrokerError::failed("invalid directory continuation".into()));
    }
    if page.rows.len() > usize::try_from(max_results).unwrap_or(usize::MAX) {
        return Err(BrokerError::failed(
            "application exceeded directory result budget".into(),
        ));
    }
    let page_actions = page
        .rows
        .iter()
        .filter(|row| row.action.is_some())
        .map(published_directory_action)
        .collect::<Result<Vec<_>, _>>()?;
    validate_resource_view_descriptors(&page_actions).map_err(BrokerError::failed)?;
    let questions = page
        .rows
        .iter()
        .map(directory_row_question)
        .collect::<Result<Vec<_>, _>>()?;
    let answers = state.permits_directory_actions(cluster_id, actor, app_id, &questions)?;
    // What is not disclosed to the actor is not listed at all; what is, and its permissions deny,
    // is listed to an agent as not allowed (`AppBrokerState::offered_to`).
    let answers = answers
        .into_iter()
        .zip(&questions)
        .map(|(answer, question)| {
            question
                .disclosed_to(actor, publishing_person)
                .then_some(answer)
        })
        .collect::<Vec<_>>();
    drop(questions);
    // Listed when the actor may take it, or would be asked first; an action
    // asking first asks for the confirmation the person gives on their
    // own device.
    page.rows = page
        .rows
        .into_iter()
        .zip(answers)
        .filter_map(|(mut row, answer)| match answer? {
            Answer::Deny if actor.is_agent() => {
                if let Some(descriptor) = row.descriptor.as_mut() {
                    descriptor.contract.availability =
                        tilo_actions::Availability::not_allowed(None);
                }
                Some(row)
            }
            Answer::Deny => None,
            Answer::Allow => Some(row),
            Answer::Ask => {
                if let Some(descriptor) = row.descriptor.as_mut()
                    && descriptor.contract.confirmation == tilo_actions::ConfirmationPolicy::None
                {
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                }
                Some(row)
            }
        })
        .collect();
    Ok(page)
}

/// Convert the already validated, exact directory row into the inert action
/// shape used by the invocation lifecycle.  This deliberately accepts only
/// an action row: containers are discoverable but never invokable.
pub(super) fn published_directory_action(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<PublishedAction, BrokerError> {
    let action = row
        .action
        .as_deref()
        .ok_or_else(|| BrokerError::failed("directory container is not invokable".into()))?;
    let descriptor = row
        .descriptor
        .as_ref()
        .ok_or_else(|| BrokerError::failed("directory action has no descriptor".into()))?;
    if descriptor.resource != row.resource || descriptor.action != action {
        return Err(BrokerError::failed(
            "directory descriptor address changed during conversion".into(),
        ));
    }
    if descriptor
        .semantic_identity
        .as_ref()
        .is_some_and(|identity| !identity.is_valid() || identity.kind != descriptor.resource_kind)
    {
        return Err(BrokerError::failed(
            "invalid directory resource semantic identity".into(),
        ));
    }
    Ok(descriptor.clone())
}

impl AppBrokerState {
    /// Clone the nonblocking bridge used by shell search.  Sending only
    /// enqueues inert values; the broker `LocalSet` rechecks authority when
    /// it later services the request.
    #[must_use]
    pub fn action_directory_request_queue(&self) -> ActionDirectoryRequestQueue {
        self.action_directory_bridge.clone()
    }
    #[track_caller]
    pub(super) fn refresh_action_directory_root_snapshot(&self) {
        let mut roots = self
            .action_directory_roots
            .lock()
            .map(|roots| roots.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        roots.sort_by_key(|root| root.peer_id);
        if let Ok(mut snapshot) = self.action_directory_root_snapshot.lock() {
            snapshot.roots = roots;
            snapshot.generation = snapshot.generation.wrapping_add(1);
            tracing::debug!(
                target: "tilo::publications",
                generation = snapshot.generation,
                roots = snapshot.roots.len(),
                cause = %std::panic::Location::caller(),
                "action directory roots changed"
            );
        }
    }
    /// Return one bounded page from the retained directory-root projection.
    /// This never clones or sorts the live registry on a palette query.
    #[must_use]
    pub fn action_directory_root_page(
        &self,
        after_peer: Option<u64>,
        max_results: usize,
    ) -> ActionDirectoryRootPage {
        let limit = max_results.min(Self::DIRECTORY_MAX_RESULTS as usize);
        let Ok(snapshot) = self.action_directory_root_snapshot.lock() else {
            return ActionDirectoryRootPage::default();
        };
        let start = after_peer.map_or(0, |peer| {
            snapshot.roots.partition_point(|root| root.peer_id <= peer)
        });
        let end = start.saturating_add(limit).min(snapshot.roots.len());
        let roots = snapshot.roots.get(start..end).unwrap_or_default().to_vec();
        let next_after_peer = (end < snapshot.roots.len())
            .then(|| {
                snapshot
                    .roots
                    .get(end.saturating_sub(1))
                    .map(|root| root.peer_id)
            })
            .flatten();
        ActionDirectoryRootPage {
            roots,
            next_after_peer,
            generation: snapshot.generation,
        }
    }
    /// Compatibility convenience for consumers that need just the first
    /// bounded page. New consumers should retain `next_after_peer` when
    /// presenting deeper root navigation.
    #[must_use]
    pub fn action_directory_roots(&self) -> Vec<ActionDirectoryRoot> {
        self.action_directory_root_page(None, Self::DIRECTORY_MAX_RESULTS as usize)
            .roots
    }
    /// `invoke`, handed over from a thread that must not wait for its admission: entered
    /// directly, never through the routing method that would hand it over again. Dispatch
    /// checks the original ownership. It blocks on the admission's journal write and on what
    /// the action does, so it runs on a thread that may block, never the broker's loop.
    pub(super) fn invoke_bridged(self: &Arc<Self>, invoke: EagerActionInvokeRequest) {
        let EagerActionInvokeRequest {
            caller,
            own_peer,
            launch,
            origin,
            request,
            reply,
        } = invoke;
        if reply.is_abandoned() {
            return;
        }
        action_provider::answer_with(
            self.invoke_eager(&caller, own_peer, launch.as_ref(), Some(&origin), &request),
            reply,
        );
    }
    /// Take the broker's turn once: run what other threads handed over, then
    /// drain a bounded batch on the broker `LocalSet`. The loop takes the turn
    /// each time it wakes, and a test that drives the broker takes it here.
    /// This function must only be called by the accept-loop `LocalSet`: it
    /// starts the Rc-backed RPCs there, while the request sender is safe for
    /// compositor threads.
    #[allow(
        clippy::too_many_lines,
        reason = "the bounded bridge deliberately keeps request validation, LocalSet dispatch, and reply delivery in one ordered drain turn"
    )]
    pub(super) fn take_turn(self: &Arc<Self>, directories: &Rc<LocalActionDirectoryRegistry>) {
        const MAX_DRAIN_PER_TICK: usize = ActionDirectoryRequestQueue::CAPACITY;
        for work in std::mem::take(&mut *self.turn_work.handed.lock_recover()) {
            work(self);
        }
        for _ in 0..MAX_DRAIN_PER_TICK {
            let request = match directories.bridge_rx.borrow().try_recv() {
                Ok(request) => request,
                Err(
                    std::sync::mpsc::TryRecvError::Empty
                    | std::sync::mpsc::TryRecvError::Disconnected,
                ) => break,
            };
            let state = Arc::clone(self);
            let directories = Rc::clone(directories);
            tokio::task::spawn_local(async move {
                match request {
                    // Admission waits for its journal write, and an app's open for the
                    // launcher: both block, so they run beside this loop, which goes on
                    // serving every app meanwhile.
                    ActionDirectoryBridgeRequest::InvokeEager(invoke) => {
                        tokio::task::spawn_blocking(move || state.invoke_bridged(*invoke));
                    }
                    ActionDirectoryBridgeRequest::Query {
                        actor,
                        cluster_id,
                        query,
                        mut reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = reply
                            .awaited(state.query_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                &query,
                            ))
                            .await
                            .and_then(|result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Page)
                                    .map_err(|reason| {
                                        tracing::warn!(
                                            %reason,
                                            peer = query.peer_id,
                                            "an app's action directory page was refused"
                                        );
                                        ActionDirectoryBridgeError::Unavailable
                                    })
                            });
                        reply.send(response);
                    }
                    ActionDirectoryBridgeRequest::Resolve {
                        actor,
                        cluster_id,
                        peer_id,
                        revision,
                        resource,
                        action,
                        mut reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = reply
                            .awaited(state.resolve_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                peer_id,
                                (&revision, &resource, &action),
                                true,
                            ))
                            .await
                            .and_then(|result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Resolution)
                                    .map_err(|_| ActionDirectoryBridgeError::Unavailable)
                            });
                        reply.send(response);
                    }
                }
            });
        }
    }
    /// Shell-local counterpart to the wire `queryActionDirectory` method.
    /// It forwards one selected live directory only, preserving the same
    /// broker budgets and current permission checks without synthesising an
    /// eager cross-app catalogue for every shell keystroke.
    #[allow(
        clippy::future_not_send,
        reason = "the registered directory handle is Rc-backed and is awaited on the shell LocalSet"
    )]
    pub(super) async fn query_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        query: &runtime_collab::ActionDirectoryQuery,
    ) -> Result<runtime_collab::ActionDirectoryPage, String> {
        if query.peer_id == 0
            || query.max_results == 0
            || query.max_bytes == 0
            || query.max_work == 0
        {
            return Err("directory query requires one peer and non-zero budgets".into());
        }
        validate_directory_query_input(query).map_err(|error| error.to_string())?;
        let registered = directories
            .registered(query.peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let max_results = query.max_results.min(Self::DIRECTORY_MAX_RESULTS);
        let max_bytes = query.max_bytes.min(Self::DIRECTORY_MAX_BYTES);
        let max_work = query.max_work.min(Self::DIRECTORY_MAX_WORK);
        let max_depth = query.max_depth.min(Self::DIRECTORY_MAX_DEPTH);
        let cursor = registered
            .pagination
            .source_cursor(
                actor,
                &query.revision,
                &query.needle,
                &query.parent_resource,
                &query.cursor,
            )
            .map_err(|error| error.to_string())?;
        let page = registered
            .directory
            .query(
                runtime_collab::ActionDirectoryQuery {
                    peer_id: query.peer_id,
                    revision: query.revision.clone(),
                    cursor,
                    needle: query.needle.clone(),
                    parent_resource: query.parent_resource.clone(),
                    max_results,
                    max_bytes,
                    max_work,
                    max_depth,
                    resource_name: query.resource_name.clone(),
                },
                max_bytes,
            )
            .await?;
        let mut page = filter_directory_page(
            self,
            cluster_id,
            actor,
            &registered.app_id,
            registered.publishing_person.as_deref(),
            page,
            max_results,
        )
        .map_err(|error| error.to_string())?;
        if !query.resource_name.is_empty() {
            // A name lookup is answered whole; no cursor continues it.
            page.next_cursor.clear();
        }
        registered
            .pagination
            .shield(actor, &query.needle, &query.parent_resource, &mut page);
        Ok(page)
    }
    /// Shell-local exact re-resolution. A `Found` row is policy-checked at
    /// this instant; callers must never execute from an earlier page cache.
    #[allow(
        clippy::future_not_send,
        reason = "the registered directory handle is Rc-backed and is awaited on the shell LocalSet"
    )]
    pub(super) async fn resolve_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        target: (&str, &str, &str),
        naming: bool,
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        let (revision, resource, action) = target;
        validate_directory_target_input(cluster_id, revision, resource, action, None)
            .map_err(|error| error.to_string())?;
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let decoded = registered
            .directory
            .resolve(
                (revision, resource, action),
                false,
                Self::DIRECTORY_MAX_BYTES,
            )
            .await?;
        match decoded {
            runtime_collab::ActionDirectoryResolution::Found {
                row,
                revision: resolved,
            } if row.resource == resource
                && row.action.as_deref() == Some(action)
                && permits_directory_row(
                    self,
                    cluster_id,
                    actor,
                    &registered.app_id,
                    registered.publishing_person.as_deref(),
                    &row,
                )
                .map_err(|error| error.to_string())? =>
            {
                // A named revision is the one resolved; only an empty one
                // takes the owner's word for which revision it read, and
                // only in the shape a caller could have named itself.
                let resolved = if revision.is_empty() {
                    resolved.filter(|resolved| {
                        validate_directory_identifier(
                            resolved,
                            "resolved revision",
                            DIRECTORY_MAX_QUERY_REVISION_BYTES,
                            true,
                        )
                        .is_ok()
                    })
                } else {
                    Some(revision.to_owned())
                };
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row,
                    revision: resolved,
                })
            }
            // An agent naming an effect only the publisher's screen shows is
            // told whose it is; it is never listed to it, nor started.
            runtime_collab::ActionDirectoryResolution::Found { mut row, .. }
                if naming
                    && row.resource == resource
                    && row.action.as_deref() == Some(action)
                    && directory_row_question(&row)
                        .is_ok_and(|question| question.refused_by_name_to(actor)) =>
            {
                if let Some(descriptor) = row.descriptor.as_mut() {
                    descriptor.contract.availability = tilo_actions::Availability::shown_here();
                }
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row,
                    revision: (!revision.is_empty()).then(|| revision.to_owned()),
                })
            }
            runtime_collab::ActionDirectoryResolution::Found { .. } => {
                Ok(runtime_collab::ActionDirectoryResolution::NotFound)
            }
            other => Ok(other),
        }
    }
    /// Resolve only a retained pre-input snapshot; never used for delivery.
    #[expect(
        clippy::future_not_send,
        reason = "directory capabilities are Rc-backed on the broker LocalSet"
    )]
    pub(super) async fn resolve_local_document_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        target: (&str, &str, &str),
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        let (revision, resource, action) = target;
        validate_directory_target_input(cluster_id, revision, resource, action, None)
            .map_err(|error| error.to_string())?;
        if revision.is_empty() {
            return Err("local admission requires an exact retained revision".into());
        }
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let decoded = registered
            .directory
            .resolve(
                (revision, resource, action),
                true,
                Self::DIRECTORY_MAX_BYTES,
            )
            .await?;
        match decoded {
            runtime_collab::ActionDirectoryResolution::Found {
                row,
                revision: resolved,
            } if resolved.as_deref() == Some(revision)
                && row.resource == resource
                && row.action.as_deref() == Some(action)
                && permits_directory_row(
                    self,
                    cluster_id,
                    actor,
                    &registered.app_id,
                    registered.publishing_person.as_deref(),
                    &row,
                )
                .map_err(|error| error.to_string())? =>
            {
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row,
                    revision: resolved,
                })
            }
            runtime_collab::ActionDirectoryResolution::Found { .. } => {
                Ok(runtime_collab::ActionDirectoryResolution::NotFound)
            }
            other => Ok(other),
        }
    }
    /// Decide validated directory rows against the current workspace
    /// authority, not the authority that existed when a cursor was issued.
    /// Every row of one page is decided on one committed sample, whatever
    /// the page's length; the borrow ends before the reply is serialized.
    pub(super) fn permits_directory_actions(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        app_id: &str,
        questions: &[DirectoryQuestion<'_>],
    ) -> Result<Vec<Answer>, BrokerError> {
        let now = self.policy_now_ns().map_err(BrokerError::failed)? / 1_000_000_000;
        let membership = self.membership.as_ref().ok_or_else(|| {
            BrokerError::failed("the action broker has no workspace authority".into())
        })?;
        membership
            .decide_in_workspace(cluster_id, |workspace| {
                let workspace = workspace
                    .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
                Ok::<Vec<Answer>, String>(
                    questions
                        .iter()
                        .map(|question| {
                            let node = tilo_actions::action_node(
                                &workspace.policy.node(),
                                app_id,
                                question.resource_kind,
                                question.resource,
                                question.field,
                                question.action,
                            );
                            actor.answer_in(workspace, &node, question.kind, now)
                        })
                        .collect(),
                )
            })
            .and_then(std::convert::identity)
            .map_err(BrokerError::failed)
    }
}
