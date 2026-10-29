//! Private native construction custody. It borrows the existing SDK worker;
//! neither an environment value nor a data capture is a positive authority.
#[cfg(feature = "broker-transport")]
use super::{
    Backend, BridgeState, BrokerTransport, QueuedSdkCommand, SdkCommand, enqueue_sdk_command,
    tokio_mpsc,
};
use super::{CollaborationManager, CommittedDocument, RouteSpec};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
thread_local! {static CONSTRUCTION:RefCell<Option<Weak<Construction>>> = const {RefCell::new(None)};}
struct Construction {
    manager: RefCell<Option<CollaborationManager>>,
    bridge: Option<InitialReadBridge>,
    primary: RefCell<Option<CommittedDocument>>,
    #[cfg(feature = "broker-transport")]
    creation: RefCell<std::collections::BTreeMap<String, CreationCapture>>,
    failure: RefCell<Option<String>>,
    transferring: Cell<bool>,
    component_scope: RefCell<Option<crate::wake::WakeScope>>,
}
#[cfg(feature = "broker-transport")]
struct CreationCapture {
    read: CommittedDocument,
    claimed: bool,
}
#[cfg(feature = "broker-transport")]
impl CreationCapture {
    fn new(read: CommittedDocument) -> Self {
        Self {
            read,
            claimed: false,
        }
    }
}
/// The record a component is bound to, as construction custody reads it: the
/// toolkit's callback binding (`ui::bound_dispatch::CallbackBinding`).
pub trait ComponentBinding {
    /// What [`Self::enter`] holds while a committed history is applied.
    type Entry;
    /// Why [`Self::enter`] refused.
    type EnterError: std::fmt::Debug;
    /// The route the record's admission names, when it names a valid one.
    fn admission_route(&self) -> Option<RouteSpec>;
    /// The app the component belongs to.
    fn app_id(&self) -> &str;
    /// The record the component is bound to.
    fn checkout(&self) -> &crate::record::RecordCheckout;
    /// Enter the component's owner.
    ///
    /// # Errors
    /// Why the owner refused the entry.
    fn enter(&self) -> Result<Self::Entry, Self::EnterError>;
    /// Merge a committed history into the bound record.
    ///
    /// # Errors
    /// Why the record refused it.
    fn merge_committed(&self, mutation: collab_types::DocumentMutation) -> Result<(), String>;
}

/// Why construction custody could not give a record's committed history.
/// The record store reports each as its own open error
/// (`ui::record::RecordOpenError`), in the same words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordReadError {
    /// The record is not the one the custody was taken for.
    #[error("record identity does not match its binding")]
    IdentityMismatch,
    /// The record the room sends this device only once it asks is not here
    /// yet: it was asked for.
    #[error("the record is on its way from the room")]
    Arriving,
    /// The read or its scope failed.
    #[error("record initialization failed: {0}")]
    Initialization(String),
}

/// Where construction's primary read comes from.
enum Primary {
    /// Read it now, on this thread.
    #[cfg(feature = "native")]
    Read,
    /// None is read (a component's construction).
    None,
    /// Read already, off this thread.
    Taken(Option<(PrimaryRead, CommittedDocument)>),
}

pub struct NativeConstruction {
    state: Rc<Construction>,
    previous: Option<Weak<Construction>>,
}
#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "broker-transport")]
pub fn component_history_phase_for_test(
    binding: &impl ComponentBinding,
) -> Option<(bool, bool, bool)> {
    current()
        .and_then(|construction| {
            let bridge = construction.bridge.as_ref()?;
            let route = binding.admission_route()?;
            bridge
                .state
                .document_reads
                .readiness_diagnostic(&route.channel_id())
        })
        .map(|phase| (phase.required, phase.staged, phase.applied))
}
#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "broker-transport")]
pub fn component_history_read_for_test(
    binding: &impl ComponentBinding,
) -> Result<CommittedDocument, String> {
    let construction = current().ok_or("original construction unavailable")?;
    let bridge = construction
        .bridge
        .as_ref()
        .ok_or("original read bridge unavailable")?;
    bridge
        .record(
            binding.app_id(),
            &bridge.route.cluster_id.to_string(),
            binding.checkout(),
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the app's own channel answers this record".into())
}
/// The committed read of a joined manager's primary route, to be taken on
/// any thread: an owner that must not wait on its own thread (a tilolang host
/// running other instances meanwhile) reads it on another and enters
/// construction with what it read ([`NativeConstruction::enter_read`]).
#[derive(Clone)]
pub struct PrimaryRead(InitialReadBridge);

impl PrimaryRead {
    /// The read `manager`'s primary route needs before its record is built;
    /// `None` when it needs none (a route whose reads are not committed, a
    /// manager without the broker).
    ///
    /// # Errors
    /// The manager's route is not admitted.
    pub fn of(manager: &CollaborationManager) -> Result<Option<Self>, String> {
        Ok(InitialReadBridge::from_manager(manager)?.map(Self))
    }

    /// Wait for the read, on the calling thread, for as long as the broker
    /// takes; its worker's end and the route's failure end the wait.
    ///
    /// # Errors
    /// The read failed or its worker ended.
    pub fn read(&self) -> Result<CommittedDocument, String> {
        self.0.read_primary()
    }
}

impl NativeConstruction {
    #[cfg(feature = "native")]
    pub fn enter(manager: CollaborationManager) -> Result<Self, String> {
        Self::enter_inner(manager, None, Primary::Read)
    }
    pub fn enter_component(
        manager: CollaborationManager,
        scope: crate::wake::WakeScope,
    ) -> Result<Self, String> {
        Self::enter_inner(manager, Some(scope), Primary::None)
    }
    /// Enter as [`Self::enter`] does, with the primary read `read` already
    /// taken off this thread through [`PrimaryRead`]; `None` when
    /// [`PrimaryRead::of`] found none to take.
    ///
    /// # Errors
    /// Construction is entered already, or the read is not of this manager's
    /// route.
    pub fn enter_read(
        manager: CollaborationManager,
        read: Option<(PrimaryRead, CommittedDocument)>,
    ) -> Result<Self, String> {
        Self::enter_inner(manager, None, Primary::Taken(read))
    }
    fn enter_inner(
        manager: CollaborationManager,
        scope: Option<crate::wake::WakeScope>,
        primary_read: Primary,
    ) -> Result<Self, String> {
        if current().is_some() {
            return Err("nested native construction would replace its worker".into());
        }
        let (bridge, primary) = match primary_read {
            // No state/root callback runs until the actual primary read is ready.
            #[cfg(feature = "native")]
            Primary::Read => {
                let bridge = InitialReadBridge::from_manager(&manager)?;
                let primary = bridge
                    .as_ref()
                    .map(InitialReadBridge::read_primary)
                    .transpose()?;
                (bridge, primary)
            }
            Primary::None => (InitialReadBridge::from_manager(&manager)?, None),
            Primary::Taken(read) => {
                let bridge = InitialReadBridge::from_manager(&manager)?;
                match (bridge, read) {
                    (None, None) => (None, None),
                    (Some(bridge), Some((PrimaryRead(read_by), read)))
                        if read_by.route == bridge.route =>
                    {
                        bridge.check()?;
                        (Some(bridge), Some(read))
                    }
                    _ => return Err("the primary read is not of this route".into()),
                }
            }
        };
        #[cfg(feature = "broker-transport")]
        let creations = {
            let creation = primary
                .as_ref()
                .filter(|read| read.creation_workspace().is_some())
                .cloned();
            let mut creations = std::collections::BTreeMap::new();
            if let (Some(read), Some(bridge)) = (creation, bridge.as_ref()) {
                creations.insert(bridge.route.channel_id(), CreationCapture::new(read));
            }
            creations
        };
        let state = Rc::new(Construction {
            manager: RefCell::new(Some(manager)),
            bridge,
            primary: RefCell::new(primary),
            #[cfg(feature = "broker-transport")]
            creation: RefCell::new(creations),
            failure: RefCell::new(None),
            transferring: Cell::new(false),
            component_scope: RefCell::new(scope),
        });
        let previous = CONSTRUCTION.with(|slot| slot.replace(Some(Rc::downgrade(&state))));
        Ok(Self { state, previous })
    }
    pub fn bind_component(&self, binding: &impl ComponentBinding) -> Result<(), String> {
        self.check()?;
        let mut manager = self.state.manager.borrow_mut();
        let manager = manager
            .as_mut()
            .ok_or("prepared component worker already transferred")?;
        // A manager still joining its room builds on the local replica: the
        // route answers later, as an event the app takes. Join the bound
        // record's route rather than the launch's, without waiting.
        if manager.is_joining() {
            let requested = binding
                .admission_route()
                .ok_or("component record route is not valid")?;
            let pending = manager
                .requested_primary_route()
                .ok_or("native component route unavailable")?;
            if requested.app_id != pending.app_id || requested.cluster_id != pending.cluster_id {
                return Err("component record does not belong to the prepared native route".into());
            }
            return manager.retarget_join(binding.checkout());
        }
        let Some(current) = manager.primary_route() else {
            return if self.state.bridge.is_none() {
                Ok(())
            } else {
                Err("native component route unavailable".into())
            };
        };
        let requested = binding
            .admission_route()
            .ok_or("component record route is not valid")?;
        if requested.app_id != current.route.app_id
            || requested.cluster_id != current.route.cluster_id
        {
            return Err("component record does not belong to the prepared native route".into());
        }
        if let Some(bridge) = &self.state.bridge {
            // Applying the capture releases its token before the independent RPC
            // worker confirms the next current read. Wait for that confirmation;
            // a fast component factory must not race the real owner recheck.
            // No clock ends the wait: the confirming read is the broker's
            // answer, which under load takes what it takes. The worker's
            // end, the route's failure and the answer each end it.
            loop {
                let seen = bridge.progress();
                bridge.check()?;
                if binding.admission_route().as_ref() != Some(&requested) {
                    return Err("component record route changed before history application".into());
                }
                bridge.apply_prepared_capture(binding, &requested)?;
                if bridge.publishable(&requested.channel_id()) {
                    break;
                }
                if let Some(error) = bridge.unreadable(&requested.channel_id()) {
                    #[cfg(feature = "broker-transport")]
                    {
                        let phase = bridge
                            .state
                            .document_reads
                            .readiness_diagnostic(&requested.channel_id());
                        tracing::warn!(
                            %error,
                            primary_route_matches = requested == current.route,
                            phase_available = phase.is_some(),
                            required = ?phase.as_ref().map(|phase| phase.required),
                            staged = ?phase.as_ref().map(|phase| phase.staged),
                            applied = ?phase.as_ref().map(|phase| phase.applied),
                            refresh_requested = ?phase.as_ref().map(|phase| phase.refresh_requested),
                            "native component history confirmation unavailable before factory"
                        );
                    }
                    return Err(format!(
                        "native component history was not confirmed before its factory: {error}"
                    ));
                }
                // The confirming read wakes this; the bound only re-checks
                // that the worker still runs.
                bridge.wait_progress(seen, crate::time::Duration::from_millis(10));
            }
        }
        manager.switch_native_record(binding.checkout())?;
        let connected = manager.is_connected();
        let current = manager.primary_route().map(|witness| witness.route.clone());
        let observed = current.as_ref().and_then(|_| binding.admission_route());
        if current
            .as_ref()
            .is_none_or(|route| observed.as_ref() != Some(route))
        {
            return Err(format!(
                "component route changed during native admission (connected={}, current_route={}, accepted_matches={}, binding_matches={})",
                connected,
                current.is_some(),
                manager
                    .primary_route
                    .accepted
                    .as_ref()
                    .is_some_and(|witness| witness.route == requested),
                observed.as_ref() == Some(&requested),
            ));
        }
        Ok(())
    }
    pub fn check(&self) -> Result<(), String> {
        if let Some(error) = self.state.failure.borrow().clone() {
            return Err(error);
        }
        if let Some(bridge) = &self.state.bridge {
            bridge.check()?;
        }
        Ok(())
    }
}
/// Retains only an actual original creation capture, never a restore heuristic.
pub struct RecordInitialization {
    read: CommittedDocument,
    #[cfg(feature = "broker-transport")]
    bridge: InitialReadBridge,
    checkout: crate::record::RecordCheckout,
}
impl RecordInitialization {
    /// Check the document `checkout` is initialized with against the
    /// original creation capture: `validate` reads the capture, and is
    /// called only for the record the capture was taken for.
    ///
    /// # Errors
    /// [`RecordReadError::IdentityMismatch`] for another record, else what
    /// `validate` found.
    pub fn validate_original<E: From<RecordReadError>>(
        &self,
        checkout: &crate::record::RecordCheckout,
        validate: impl FnOnce(&CommittedDocument) -> Result<(), E>,
    ) -> Result<(), E> {
        if checkout != &self.checkout {
            return Err(RecordReadError::IdentityMismatch.into());
        }
        validate(&self.read)
    }
    pub fn workspace(&self) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
        #[cfg(feature = "broker-transport")]
        {
            self.read.creation_workspace()
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            None
        }
    }
    pub fn commit(&self, kind: &str, document: Vec<u8>) -> Result<(), String> {
        #[cfg(feature = "broker-transport")]
        {
            let request = self.read.initialization_request(kind, document)?;
            self.bridge.initialize(request)?;
            self.read.initialized();
            Ok(())
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = (kind, document);
            Err("native initialization unavailable".into())
        }
    }
}
#[cfg(feature = "broker-transport")]
pub fn take_initialization(
    app: &str,
    room: &str,
    checkout: &crate::record::RecordCheckout,
) -> Result<Option<RecordInitialization>, String> {
    let Some(state) = current() else {
        return Ok(None);
    };
    let Some(bridge) = state.bridge.as_ref() else {
        return Ok(None);
    };
    bridge.check_scope(app, Some(room))?;
    let mut scoped = bridge.clone();
    if !checkout.id.is_package_primary() {
        scoped.route = RouteSpec::record(app, bridge.route.cluster_id, checkout.id.as_str());
    }
    let mut captures = state.creation.borrow_mut();
    let Some(capture) = captures.get_mut(&scoped.route.channel_id()) else {
        return Ok(None);
    };
    if capture.claimed {
        return Ok(None);
    }
    capture.claimed = true;
    Ok(Some(RecordInitialization {
        read: capture.read.clone(),
        bridge: scoped,
        checkout: checkout.clone(),
    }))
}
// Portable hosts cannot receive a native creation capture. Keep the public
// construction path available without ever fabricating a broker witness.
#[cfg(not(feature = "broker-transport"))]
pub fn take_initialization(
    _app: &str,
    _room: &str,
    _checkout: &crate::record::RecordCheckout,
) -> Result<Option<RecordInitialization>, String> {
    Ok(None)
}

impl NativeConstruction {
    /// Leave construction, handing back the manager and the component's
    /// wake scope when nothing took them: a launch whose record is not on
    /// this device builds again once its join is over, on the same manager
    /// in the same scope.
    pub fn into_parts(self) -> (Option<CollaborationManager>, Option<crate::wake::WakeScope>) {
        let manager = self.state.manager.borrow_mut().take();
        let scope = self.state.component_scope.borrow_mut().take();
        (manager, scope)
    }
}
/// Whether the construction under way is on a manager still joining its
/// room. A record that is not on this device is then not created: the room
/// may hold it, and a second copy made here would diverge from it. It
/// arrives with the join.
pub fn joining() -> bool {
    current().is_some_and(|state| {
        state
            .manager
            .borrow()
            .as_ref()
            .is_some_and(CollaborationManager::is_joining)
    })
}
impl Drop for NativeConstruction {
    fn drop(&mut self) {
        CONSTRUCTION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}
fn current() -> Option<Rc<Construction>> {
    CONSTRUCTION.with(|slot| slot.borrow().as_ref().and_then(Weak::upgrade))
}
pub fn check_current() -> Result<(), String> {
    let Some(state) = current() else {
        return Ok(());
    };
    if let Some(error) = state.failure.borrow().clone() {
        return Err(error);
    }
    if let Some(bridge) = &state.bridge {
        bridge.check()?;
    }
    if state.primary.borrow().is_some() {
        return Err("native state opener did not consume the required initial history".into());
    }
    Ok(())
}
#[cfg(feature = "native")]
pub fn ensure_unoccupied() -> Result<(), String> {
    if current().is_some() {
        let error = "nested native launch cannot replace the prepared worker";
        fail(error);
        return Err(error.into());
    }
    Ok(())
}
pub fn transfer_to_app<T>(construct: impl FnOnce() -> T) -> Result<T, String> {
    let Some(state) = current() else {
        return Ok(construct());
    };
    check_current()?;
    if state.manager.borrow().is_none() {
        return Err("native construction worker was already transferred".into());
    }
    struct Transfer(Rc<Construction>);
    impl Drop for Transfer {
        fn drop(&mut self) {
            self.0.transferring.set(false);
        }
    }
    state.transferring.set(true);
    let _transfer = Transfer(state);
    Ok(construct())
}
pub fn take_component_scope() -> Option<crate::wake::WakeScope> {
    let state = current()?;
    if state.transferring.get() {
        return state.component_scope.borrow_mut().take();
    }
    if state.component_scope.borrow().is_some() {
        fail("nested component cannot take native wake custody");
    }
    None
}
pub fn take_manager() -> Option<CollaborationManager> {
    let state = current()?;
    if state.transferring.replace(false) {
        if let Some(manager) = state.manager.borrow_mut().take() {
            return Some(manager);
        }
    }
    fail("App construction attempted to take the prepared worker outside final launch");
    // A nested App must never fall through to ambient FD discovery.
    Some(CollaborationManager::disabled())
}
pub fn capture_bridge() -> Option<InitialReadBridge> {
    current()?.bridge.clone()
}
/// Capture the component's already-bound route, while retaining the original worker.
pub fn service_capture_bridge() -> Option<InitialReadBridge> {
    let construction = current()?;
    if construction.failure.borrow().is_some() {
        return None;
    }
    let mut bridge = construction.bridge.clone()?;
    let route = construction
        .manager
        .borrow()
        .as_ref()?
        .primary_route()?
        .route
        .clone();
    if route.record.is_none()
        || route.app_id != bridge.route.app_id
        || route.cluster_id != bridge.route.cluster_id
    {
        return None;
    }
    bridge
        .check_scope("so.tilo.browser", Some(&route.cluster_id.to_string()))
        .ok()?;
    if !bridge.publishable(&route.channel_id()) {
        return None;
    }
    bridge.route = route;
    Some(bridge)
}
pub fn fail(error: &str) {
    if let Some(state) = current() {
        state
            .failure
            .borrow_mut()
            .get_or_insert_with(|| error.into());
    }
}
pub fn primary(
    app: &str,
    room: Option<&str>,
) -> Option<Result<(String, CommittedDocument), String>> {
    let state = current()?;
    let bridge = state.bridge.as_ref()?;
    Some((|| {
        bridge.check_scope(app, room)?;
        let read = state
            .primary
            .borrow_mut()
            .take()
            .map(Ok)
            .unwrap_or_else(|| bridge.read_primary())?;
        Ok((bridge.route.cluster_id.to_string(), read))
    })())
}
#[cfg(feature = "broker-transport")]
#[derive(Clone)]
pub struct InitialReadBridge {
    route: RouteSpec,
    commands: tokio_mpsc::WeakSender<QueuedSdkCommand>,
    state: BridgeState,
}
#[cfg(feature = "broker-transport")]
impl InitialReadBridge {
    pub fn service_request(
        &self,
        command: runtime_collab::BrowserServiceSessionCommand,
        wake: Option<crate::WakeHandle>,
    ) -> Result<
        std::sync::mpsc::Receiver<Result<runtime_collab::BrowserServiceSessionReply, String>>,
        String,
    > {
        self.check_scope("so.tilo.browser", None)?;
        if !command.valid() {
            return Err("This account could not be opened".into());
        }
        if !self.publishable(&self.route.channel_id()) {
            return Err("This account could not be opened".into());
        }
        let tx = self
            .commands
            .upgrade()
            .ok_or("This account could not be opened")?;
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &tx,
            SdkCommand::BrowserServiceSession {
                route: self.route.clone(),
                command,
                wake,
                reply,
            },
            &self.state,
        ) {
            return Err("This account could not be opened".into());
        }
        Ok(received)
    }
    fn initialize(
        &self,
        request: runtime_collab::committed_document::Request,
    ) -> Result<(), String> {
        self.check()?;
        let tx = self
            .commands
            .upgrade()
            .ok_or("native initialization worker ended")?;
        let (reply, received) = std::sync::mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &tx,
            SdkCommand::Initialize {
                route: self.route.clone(),
                request,
                reply,
            },
            &self.state,
        ) {
            return Err("native initialization queue unavailable".into());
        }
        // No clock ends the wait: the worker answers once the broker has
        // committed the initialization, or it failed. A worker that ends
        // drops the reply unanswered; the bound only re-checks custody.
        loop {
            self.check()?;
            match received.recv_timeout(crate::time::Duration::from_millis(10)) {
                Ok(result) => return result.map(|_| ()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("native initialization reply ended".into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    fn from_manager(manager: &CollaborationManager) -> Result<Option<Self>, String> {
        let Backend::Transport(transport) = &manager.backend else {
            return Ok(None);
        };
        // A manager still joining its room builds on the local replica (the
        // non-Required path): its route's committed history arrives later,
        // as an event the app merges.
        if manager.awaiting_primary {
            return Ok(None);
        }
        let Some(channel) = transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
        else {
            return Ok(None);
        };
        let route = manager
            .primary_route
            .accepted
            .as_ref()
            .ok_or("native primary route is not admitted")?
            .route
            .clone();
        if !channel.state.document_reads.required(&route.channel_id()) {
            return Ok(None);
        }
        Ok(Some(Self {
            route,
            commands: channel.outbound_tx.downgrade(),
            state: channel.state.clone(),
        }))
    }
    fn check(&self) -> Result<(), String> {
        if !self.state.is_healthy() || self.commands.upgrade().is_none() {
            return Err("native construction worker ended".into());
        }
        Ok(())
    }
    fn publishable(&self, channel: &str) -> bool {
        self.state.document_reads.publishable(channel)
    }
    /// Why no read of `channel` is coming, once none is.
    fn unreadable(&self, channel: &str) -> Option<String> {
        self.state.document_reads.unreadable(channel)
    }
    fn apply_prepared_capture(
        &self,
        binding: &impl ComponentBinding,
        requested: &RouteSpec,
    ) -> Result<(), String> {
        let Some(read) = self.state.document_reads.unapplied(&requested.channel_id()) else {
            return Ok(());
        };
        self.check_scope(&requested.app_id, Some(&requested.cluster_id.to_string()))?;
        if binding.admission_route().as_ref() != Some(requested) {
            return Err("component record route changed before history application".into());
        }
        // Creation remains owned by the original receipt/initialization path.
        // A staged replacement must never materialize a reservation implicitly.
        if read.creation_workspace().is_some() {
            return Err("original application creation is not initialized".into());
        }
        let _entry = binding
            .enter()
            .map_err(|error| format!("component owner history admission refused: {error:?}"))?;
        read.with_mutation(|mutation| binding.merge_committed(mutation.clone()))??;
        self.check()?;
        if !read.is_current()
            || !self.state.document_reads.required(&requested.channel_id())
            || binding.admission_route().as_ref() != Some(requested)
        {
            return Err("component record route changed during history application".into());
        }
        read.applied();
        Ok(())
    }
    fn check_scope(&self, app: &str, room: Option<&str>) -> Result<(), String> {
        self.check()?;
        if app != self.route.app_id
            || room.is_some_and(|room| room != self.route.cluster_id.to_string())
        {
            return Err("native initial read scope mismatch".into());
        }
        Ok(())
    }
    /// The committed history `checkout` starts from, read before the app
    /// builds its record. `None` when no committed read answers it: the
    /// broker said the app's own channel does (a record a sealed room does
    /// not hold yet, which is new and which the app makes there), and the
    /// record opens as one without a bridge would. A record the room sends
    /// this device only once it asks
    /// ([`crate::record::RecordId::arrives_on_demand`]) is asked for and
    /// never waited on: [`RecordReadError::Arriving`] until it
    /// is here, and the store's readers are woken when it arrives. One the
    /// app's own channel answers never arrives committed: it opens without a
    /// committed read too, rather than arriving forever.
    pub fn record(
        &self,
        app: &str,
        room: &str,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<Option<CommittedDocument>, RecordReadError> {
        if checkout.id.arrives_on_demand() {
            self.check_scope(app, Some(room))
                .map_err(RecordReadError::Initialization)?;
            if self.answered_by_own_channel(checkout) {
                return Ok(None);
            }
            return self
                .arrived(checkout)
                .map_err(RecordReadError::Initialization)?
                .map(Some)
                .ok_or(RecordReadError::Arriving);
        }
        self.record_waiting(app, room, checkout)
            .map_err(RecordReadError::Initialization)
    }
    fn record_waiting(
        &self,
        app: &str,
        room: &str,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<Option<CommittedDocument>, String> {
        self.check_scope(app, Some(room))?;
        let route = if checkout.id.is_package_primary() {
            self.route.clone()
        } else {
            RouteSpec::record(app, self.route.cluster_id, checkout.id.as_str())
        };
        if let Some(read) = current()
            .and_then(|state| {
                state
                    .creation
                    .borrow()
                    .get(&route.channel_id())
                    .map(|capture| capture.read.clone())
            })
            .filter(|read| read.creation_workspace().is_some())
        {
            // A receiptless refresh cannot replace the source of first
            // materialization. But it must not conceal newer owner history.
            if let Some(observed) = self.state.document_reads.initial(&route.channel_id()) {
                if captured_heads(&read)? != captured_heads(&observed)? {
                    return Err("original creation graph changed before initialization".into());
                }
                observed.applied();
            }
            return Ok(Some(read));
        }
        let read = if checkout.id.is_package_primary() {
            if let Some(initial) = primary(app, Some(room)) {
                Some(initial.map(|(_, read)| read)?)
            } else {
                self.read(None)?
            }
        } else {
            self.read(Some(checkout))?
        };
        let Some(read) = read else {
            return Ok(None);
        };
        if read.creation_workspace().is_some()
            && let Some(state) = current()
        {
            state
                .creation
                .borrow_mut()
                .entry(route.channel_id())
                .or_insert_with(|| CreationCapture::new(read.clone()));
        }
        Ok(Some(read))
    }
    /// Whether the broker answered `checkout`'s read as the app's own
    /// channel's: a record a sealed room does not hold yet, which is new and
    /// which the app makes there. Never the primary route, which the bridge
    /// reads committed.
    pub fn answered_by_own_channel(&self, checkout: &crate::record::RecordCheckout) -> bool {
        !checkout.id.is_package_primary()
            && self.state.document_reads.answered_by_own_channel(
                &RouteSpec::record(
                    self.route.app_id.clone(),
                    self.route.cluster_id,
                    checkout.id.as_str(),
                )
                .channel_id(),
            )
    }
    /// The committed read of the primary route. A bridge exists only for an
    /// app whose primary route is read committed, so it is never answered by
    /// the app's own channel.
    fn read_primary(&self) -> Result<CommittedDocument, String> {
        self.read(None)?
            .ok_or_else(|| "the primary route is answered by the app's own channel".into())
    }
    /// The committed read of the primary route, or of `checkout`'s record:
    /// `None` once the broker answered that the app's own channel serves it.
    fn read(
        &self,
        checkout: Option<&crate::record::RecordCheckout>,
    ) -> Result<Option<CommittedDocument>, String> {
        self.check()?;
        let route = checkout.map_or_else(
            || self.route.clone(),
            |checkout| {
                RouteSpec::record(
                    self.route.app_id.clone(),
                    self.route.cluster_id,
                    checkout.id.as_str(),
                )
            },
        );
        let record_kind = checkout.map(|checkout| checkout.kind.as_str().to_owned());
        let key = route.channel_id();
        if self.state.document_reads.joining(&key) {
            let tx = self.commands.upgrade().ok_or("native read worker ended")?;
            if !enqueue_sdk_command(&tx, SdkCommand::Join(route, record_kind), &self.state) {
                return Err("native initial read queue unavailable".into());
            }
        }
        // No clock ends this wait: the broker answers the read once it can,
        // which for a record of a sealed room can mean after the room's
        // host replayed it and a participant of the channel admitted this device
        // to its keys. Each of those is an event the broker waits on; under
        // load they take what they take. The worker's end, the route's
        // failure and the broker's answer each wake it.
        loop {
            let seen = self.progress();
            self.check()?;
            if let Some(read) = self.state.document_reads.initial(&key) {
                return Ok(Some(read));
            }
            if self.state.document_reads.answered_by_own_channel(&key) {
                return Ok(None);
            }
            if let Some(error) = self.state.document_reads.unreadable(&key) {
                return Err(format!("native initial history unavailable: {error}"));
            }
            // The RPC worker has a separate runtime. No lock or application
            // document borrow crosses this construction wait; staging the
            // capture ends it at once, and the bound only re-checks that the
            // worker still runs.
            self.wait_progress(seen, crate::time::Duration::from_millis(10));
        }
    }
    /// The committed history of the on-demand record `checkout` that
    /// arrived, asking the room for it the first time: `None` until it is
    /// here. Its arrival reaches the app as every committed read does
    /// ([`super::CollabEvent::CommittedDocument`]), which wakes the store's
    /// readers.
    fn arrived(
        &self,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<Option<CommittedDocument>, String> {
        self.check()?;
        let route = RouteSpec::record(
            self.route.app_id.clone(),
            self.route.cluster_id,
            checkout.id.as_str(),
        );
        let key = route.channel_id();
        if self.state.document_reads.joining(&key) {
            let tx = self.commands.upgrade().ok_or("native read worker ended")?;
            let kind = checkout.kind.as_str().to_owned();
            if !enqueue_sdk_command(&tx, SdkCommand::Join(route, Some(kind)), &self.state) {
                return Err("native record read queue unavailable".into());
            }
        }
        Ok(self.state.document_reads.initial(&key))
    }
    fn progress(&self) -> u64 {
        self.state.document_reads.progress()
    }
    fn wait_progress(&self, seen: u64, bound: crate::time::Duration) {
        self.state.document_reads.wait_progress(seen, bound);
    }
}

#[cfg(feature = "broker-transport")]
fn captured_heads(read: &CommittedDocument) -> Result<Vec<automerge::ChangeHash>, String> {
    read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err("creation capture is not a full committed graph".into());
        };
        automerge::AutoCommit::load(bytes)
            .map(|document| automerge::transaction::Transactable::base_heads(&document))
            .map_err(|error| error.to_string())
    })?
}

/// Without the broker transport there is no SDK worker to read committed
/// history from, so no bridge can exist: the portable host always takes the
/// non-Required path.
#[cfg(not(feature = "broker-transport"))]
#[derive(Clone)]
pub struct InitialReadBridge {
    never: std::convert::Infallible,
    route: RouteSpec,
}
#[cfg(not(feature = "broker-transport"))]
impl InitialReadBridge {
    pub fn service_request(
        &self,
        _command: runtime_collab::BrowserServiceSessionCommand,
        _wake: Option<crate::WakeHandle>,
    ) -> Result<
        std::sync::mpsc::Receiver<Result<runtime_collab::BrowserServiceSessionReply, String>>,
        String,
    > {
        match self.never {}
    }
    fn from_manager(_manager: &CollaborationManager) -> Result<Option<Self>, String> {
        Ok(None)
    }
    fn check(&self) -> Result<(), String> {
        match self.never {}
    }
    fn check_scope(&self, _app: &str, _room: Option<&str>) -> Result<(), String> {
        match self.never {}
    }
    fn publishable(&self, _channel: &str) -> bool {
        match self.never {}
    }
    fn unreadable(&self, _channel: &str) -> Option<String> {
        match self.never {}
    }
    fn progress(&self) -> u64 {
        match self.never {}
    }
    fn wait_progress(&self, _seen: u64, _bound: crate::time::Duration) {
        match self.never {}
    }
    fn apply_prepared_capture(
        &self,
        _binding: &impl ComponentBinding,
        _requested: &RouteSpec,
    ) -> Result<(), String> {
        match self.never {}
    }
    pub fn record(
        &self,
        _app: &str,
        _room: &str,
        _checkout: &crate::record::RecordCheckout,
    ) -> Result<Option<CommittedDocument>, RecordReadError> {
        match self.never {}
    }
    fn read_primary(&self) -> Result<CommittedDocument, String> {
        match self.never {}
    }
    pub fn answered_by_own_channel(&self, _checkout: &crate::record::RecordCheckout) -> bool {
        match self.never {}
    }
}

#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "broker-transport")]
#[path = "native_initial/named_creation.rs"]
pub mod named_creation;

#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod tests {
    use super::{
        BridgeState, CollaborationManager, Construction, InitialReadBridge, NativeConstruction,
        RouteSpec, take_initialization,
    };
    use crate::broker::{ByteBudget, committed_document::read_capture_using};
    use runtime_collab::committed_document::{
        CreationChunk, CreationReceipt, CreationWorkspace, Reply,
    };
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        rc::Rc,
    };

    struct BoundTransport;
    impl crate::CollabTransport for BoundTransport {
        fn status(&self) -> crate::TransportStatus {
            crate::TransportStatus::Ready
        }
        fn submit(&mut self, _: crate::TransportCommand) -> Result<(), crate::TransportError> {
            Ok(())
        }
        fn drain(&mut self, _: &mut Vec<crate::CollabEvent>) {}
        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            None
        }
        fn take_action_directory_requests(&mut self, _: &mut Vec<crate::ActionDirectoryRequest>) {}
    }

    #[tokio::test(flavor = "current_thread")]
    async fn browser_session_private_receipt_stays_on_original_route_and_refuses_revoked_reads() {
        let route = RouteSpec::record("so.tilo.browser", uuid::Uuid::new_v4(), "browser-original");
        let state = BridgeState::new();
        let ready = state.document_reads.close_for_test(&route.channel_id());
        ready.store(true, std::sync::atomic::Ordering::Release);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let bridge = InitialReadBridge {
            route: RouteSpec::app("so.tilo.browser", route.cluster_id),
            commands: sender.downgrade(),
            state: state.clone(),
        };
        let mut manager = CollaborationManager::with_transport(Box::new(BoundTransport));
        manager.primary_route.announce(route.clone());
        manager.primary_route.accept(Ok(route.clone())).unwrap();
        assert_eq!(manager.primary_route().unwrap().route, route);
        let construction = Rc::new(Construction {
            manager: RefCell::new(Some(manager)),
            bridge: Some(bridge.clone()),
            primary: RefCell::new(None),
            creation: RefCell::new(BTreeMap::new()),
            failure: RefCell::new(None),
            transferring: Cell::new(false),
            component_scope: RefCell::new(None),
        });
        let previous =
            super::CONSTRUCTION.with(|slot| slot.replace(Some(Rc::downgrade(&construction))));
        let _construction = NativeConstruction {
            state: construction,
            previous,
        };
        let client = crate::browser_service_sessions::Client::capture().unwrap();
        let later_route = RouteSpec::record("so.tilo.browser", route.cluster_id, "later-record");
        state
            .document_reads
            .close_for_test(&later_route.channel_id())
            .store(true, std::sync::atomic::Ordering::Release);
        {
            let mut retained = _construction.state.manager.borrow_mut();
            let manager = retained.as_mut().unwrap();
            manager.primary_route.announce(later_route.clone());
            manager
                .primary_route
                .accept(Ok(later_route.clone()))
                .unwrap();
            assert_eq!(manager.primary_route().unwrap().route, later_route);
        }
        let mut received = client
            .request(runtime_collab::BrowserServiceSessionCommand::List, None)
            .unwrap();
        let queued = receiver.try_recv().unwrap();
        let super::SdkCommand::BrowserServiceSession {
            route: actual,
            command,
            reply,
            ..
        } = queued.command
        else {
            panic!("private request entered an ordinary document command");
        };
        assert_eq!(actual, route);
        assert!(matches!(
            command,
            runtime_collab::BrowserServiceSessionCommand::List
        ));
        assert!(received.poll().is_none());
        reply
            .try_send(Ok(runtime_collab::BrowserServiceSessionReply::List {
                sessions: vec![],
            }))
            .unwrap();
        assert!(
            matches!(received.poll().unwrap().unwrap(), runtime_collab::BrowserServiceSessionReply::List { sessions } if sessions.is_empty())
        );
        assert!(received.poll().is_none(), "private reply delivered twice");
        let next = client
            .request(runtime_collab::BrowserServiceSessionCommand::List, None)
            .unwrap();
        drop(received);
        assert!(
            client
                .request(runtime_collab::BrowserServiceSessionCommand::List, None)
                .is_err(),
            "old receipt released new in-flight custody"
        );
        let _second_queued = receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
        drop(next);
        let scope = crate::WakeScope::new();
        let retired = scope.handle();
        drop(scope);
        assert!(!retired.is_active());
        assert!(
            client
                .request(
                    runtime_collab::BrowserServiceSessionCommand::List,
                    Some(retired)
                )
                .is_err()
        );
        assert!(
            receiver.try_recv().is_err(),
            "retired component queued a private request"
        );
        let other = InitialReadBridge {
            route: RouteSpec::record("so.tilo.chat", route.cluster_id, "browser-original"),
            commands: sender.downgrade(),
            state: state.clone(),
        };
        assert!(
            other
                .service_request(runtime_collab::BrowserServiceSessionCommand::List, None)
                .is_err()
        );
        state.document_reads.close_for_test(&route.channel_id());
        assert!(
            client
                .request(runtime_collab::BrowserServiceSessionCommand::List, None)
                .is_err()
        );
        assert!(
            receiver.try_recv().is_err(),
            "refused routes queued private requests"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn component_primary_read_retains_original_creation_receipt_without_eager_read() {
        let room = uuid::Uuid::new_v4();
        let route = RouteSpec::app("so.tilo.chat", room);
        let workspace = CreationWorkspace {
            id: room.to_string(),
            name: "Verified room".into(),
        };
        let first = Reply::Created {
            receipt: CreationReceipt {
                token: [7; 16],
                workspace: workspace.clone(),
            },
            chunk: CreationChunk {
                capture: 1,
                revision: [3; 32],
                total: 1,
                bytes: vec![1],
                token: [4; 16],
            },
        };
        let (read, _) = read_capture_using(first, &ByteBudget::new(), async |request| {
            assert_eq!(
                request,
                runtime_collab::committed_document::Request::Cancel {
                    capture: 1,
                    token: [4; 16],
                }
            );
            Ok(Reply::Cancelled)
        })
        .await
        .unwrap();
        let bridge_state = BridgeState::new();
        bridge_state
            .document_reads
            .stage_for_test(&route.channel_id(), read);
        let (sender, _receiver) = tokio::sync::mpsc::channel(4);
        let bridge = InitialReadBridge {
            route,
            commands: sender.downgrade(),
            state: bridge_state,
        };
        let state = Rc::new(Construction {
            manager: RefCell::new(Some(CollaborationManager::disabled())),
            bridge: Some(bridge.clone()),
            primary: RefCell::new(None),
            creation: RefCell::new(BTreeMap::new()),
            failure: RefCell::new(None),
            transferring: Cell::new(false),
            component_scope: RefCell::new(None),
        });
        let previous = super::CONSTRUCTION.with(|slot| slot.replace(Some(Rc::downgrade(&state))));
        let _construction = NativeConstruction { state, previous };
        let checkout = crate::record::RecordCheckout::new(
            crate::record::RecordId::package_primary(),
            crate::record::RecordKind::new("chat.workspace"),
        );
        bridge
            .record("so.tilo.chat", &room.to_string(), &checkout)
            .unwrap()
            .unwrap()
            .applied();
        let initialization = take_initialization("so.tilo.chat", &room.to_string(), &checkout)
            .unwrap()
            .unwrap();
        assert_eq!(initialization.workspace(), Some(&workspace));
        assert!(
            take_initialization("so.tilo.chat", &room.to_string(), &checkout)
                .unwrap()
                .is_none()
        );
    }

    /// A record a sealed room does not hold yet is new, and the app makes it
    /// on its own channel: the broker answers its read so ([`Reply::Legacy`]).
    /// The bridge says no committed read answers it, at once, rather than
    /// waiting out its deadline for a read that never comes and failing the
    /// app's open of the record.
    #[test]
    fn a_record_the_apps_own_channel_answers_has_no_committed_read() {
        let room = uuid::Uuid::new_v4();
        let checkout = crate::record::RecordCheckout::new(
            crate::record::RecordId::parse(
                "derived:part.0@derived:messages@derived:default@tab:0000000000000000",
            )
            .expect("a record id"),
            crate::record::RecordKind::new("chat.part"),
        );
        let route = RouteSpec::record("so.tilo.chat", room, checkout.id.as_str());
        let state = BridgeState::new();
        state.document_reads.legacy_for_test(&route.channel_id());
        let (sender, _receiver) = tokio::sync::mpsc::channel(4);
        let bridge = InitialReadBridge {
            route: RouteSpec::app("so.tilo.chat", room),
            commands: sender.downgrade(),
            state,
        };
        let started = std::time::Instant::now();
        assert!(
            bridge
                .record("so.tilo.chat", &room.to_string(), &checkout)
                .unwrap()
                .is_none()
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "answered without waiting out the read's deadline"
        );
    }

    /// A read waits for the broker's answer, however long that takes, and
    /// no longer once none is coming: a route whose join failed says why to
    /// the read waiting for it, at once.
    #[test]
    fn a_read_of_a_route_that_did_not_join_fails_with_its_reason() {
        let room = uuid::Uuid::new_v4();
        let checkout = crate::record::RecordCheckout::new(
            crate::record::RecordId::parse("uuid:00000000-0000-0000-0000-000000000001")
                .expect("a record id"),
            crate::record::RecordKind::new("chat.part"),
        );
        let route = RouteSpec::record("so.tilo.chat", room, checkout.id.as_str());
        let state = BridgeState::new();
        let reads = state.document_reads.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<super::QueuedSdkCommand>(4);
        // The worker: it takes the read's join and fails it.
        let worker = std::thread::spawn(move || {
            let queued = receiver
                .blocking_recv()
                .expect("the read asks to join its route");
            let super::SdkCommand::Join(joined, _) = queued.command else {
                panic!("the read asks to join its route");
            };
            assert_eq!(joined.channel_id(), route.channel_id());
            reads.join_failed(&joined.channel_id(), "the broker refused the route".into());
            receiver
        });
        let bridge = InitialReadBridge {
            route: RouteSpec::app("so.tilo.chat", room),
            commands: sender.downgrade(),
            state,
        };
        let refused = bridge
            .record("so.tilo.chat", &room.to_string(), &checkout)
            .expect_err("no read is coming");
        assert!(
            refused.to_string().contains("the broker refused the route"),
            "the read says why: {refused}"
        );
        drop(worker.join().expect("the worker ran"));
        drop(sender);
    }
}
