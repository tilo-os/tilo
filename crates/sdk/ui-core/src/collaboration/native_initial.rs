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
pub(crate) struct NativeConstruction {
    state: Rc<Construction>,
    previous: Option<Weak<Construction>>,
}
#[cfg(test)]
#[cfg(feature = "broker-transport")]
pub(crate) fn component_history_phase_for_test(
    binding: &crate::bound_dispatch::CallbackBinding,
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
#[cfg(test)]
#[cfg(feature = "broker-transport")]
pub(crate) fn component_history_read_for_test(
    binding: &crate::bound_dispatch::CallbackBinding,
) -> Result<CommittedDocument, String> {
    let construction = current().ok_or("original construction unavailable")?;
    let bridge = construction
        .bridge
        .as_ref()
        .ok_or("original read bridge unavailable")?;
    bridge.record(
        &binding.app_id,
        &bridge.route.cluster_id.to_string(),
        &binding.checkout,
    )
}
impl NativeConstruction {
    #[cfg(feature = "native")]
    pub(crate) fn enter(manager: CollaborationManager) -> Result<Self, String> {
        Self::enter_inner(manager, None, true)
    }
    pub(crate) fn enter_component(
        manager: CollaborationManager,
        scope: crate::wake::WakeScope,
    ) -> Result<Self, String> {
        Self::enter_inner(manager, Some(scope), false)
    }
    fn enter_inner(
        manager: CollaborationManager,
        scope: Option<crate::wake::WakeScope>,
        primary_read: bool,
    ) -> Result<Self, String> {
        if current().is_some() {
            return Err("nested native construction would replace its worker".into());
        }
        let bridge = InitialReadBridge::from_manager(&manager)?;
        // No state/root callback runs until the actual primary read is ready.
        let primary = if primary_read {
            bridge
                .as_ref()
                .map(|bridge| bridge.read(None))
                .transpose()?
        } else {
            None
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
    pub(crate) fn bind_component(
        &self,
        binding: &crate::bound_dispatch::CallbackBinding,
    ) -> Result<(), String> {
        self.check()?;
        let mut manager = self.state.manager.borrow_mut();
        let manager = manager
            .as_mut()
            .ok_or("prepared component worker already transferred")?;
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
            let deadline = crate::time::now() + crate::time::Duration::from_secs(3);
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
                if crate::time::now() >= deadline {
                    #[cfg(feature = "broker-transport")]
                    {
                        let phase = bridge
                            .state
                            .document_reads
                            .readiness_diagnostic(&requested.channel_id());
                        tracing::warn!(
                            primary_route_matches = requested == current.route,
                            phase_available = phase.is_some(),
                            required = ?phase.as_ref().map(|phase| phase.required),
                            staged = ?phase.as_ref().map(|phase| phase.staged),
                            applied = ?phase.as_ref().map(|phase| phase.applied),
                            refresh_requested = ?phase.as_ref().map(|phase| phase.refresh_requested),
                            "native component history confirmation unavailable before factory"
                        );
                    }
                    return Err(
                        "native component history was not confirmed before its factory".into(),
                    );
                }
                // The confirming read wakes this; the bound re-checks the
                // worker's health and the deadline.
                bridge.wait_progress(seen, crate::time::Duration::from_millis(10));
            }
        }
        manager.switch_native_record(&binding.checkout)?;
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
    pub(crate) fn check(&self) -> Result<(), String> {
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
pub(crate) struct RecordInitialization {
    read: CommittedDocument,
    #[cfg(feature = "broker-transport")]
    bridge: InitialReadBridge,
    checkout: crate::record::RecordCheckout,
}
impl RecordInitialization {
    pub(crate) fn validate_original(
        &self,
        document: &automerge::AutoCommit,
        checkout: &crate::record::RecordCheckout,
        workspace: &str,
    ) -> Result<(), crate::record::RecordOpenError> {
        if checkout != &self.checkout {
            return Err(crate::record::RecordOpenError::IdentityMismatch);
        }
        crate::record::validate_creation_capture(document, checkout, workspace, &self.read)
    }
    pub(crate) fn workspace(
        &self,
    ) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
        #[cfg(feature = "broker-transport")]
        {
            self.read.creation_workspace()
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            None
        }
    }
    pub(crate) fn commit(&self, kind: &str, document: Vec<u8>) -> Result<(), String> {
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
pub(crate) fn take_initialization(
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
pub(crate) fn take_initialization(
    _app: &str,
    _room: &str,
    _checkout: &crate::record::RecordCheckout,
) -> Result<Option<RecordInitialization>, String> {
    Ok(None)
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
pub(crate) fn check_current() -> Result<(), String> {
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
pub(crate) fn ensure_unoccupied() -> Result<(), String> {
    if current().is_some() {
        let error = "nested native launch cannot replace the prepared worker";
        fail(error);
        return Err(error.into());
    }
    Ok(())
}
pub(crate) fn transfer_to_app<T>(construct: impl FnOnce() -> T) -> Result<T, String> {
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
pub(crate) fn take_component_scope() -> Option<crate::wake::WakeScope> {
    let state = current()?;
    if state.transferring.get() {
        return state.component_scope.borrow_mut().take();
    }
    if state.component_scope.borrow().is_some() {
        fail("nested component cannot take native wake custody");
    }
    None
}
pub(crate) fn take_manager() -> Option<CollaborationManager> {
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
pub(crate) fn capture_bridge() -> Option<InitialReadBridge> {
    current()?.bridge.clone()
}
pub(crate) fn fail(error: &str) {
    if let Some(state) = current() {
        state
            .failure
            .borrow_mut()
            .get_or_insert_with(|| error.into());
    }
}
pub(crate) fn primary(
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
            .unwrap_or_else(|| bridge.read(None))?;
        Ok((bridge.route.cluster_id.to_string(), read))
    })())
}
#[cfg(feature = "broker-transport")]
#[derive(Clone)]
pub(crate) struct InitialReadBridge {
    route: RouteSpec,
    commands: tokio_mpsc::WeakSender<QueuedSdkCommand>,
    state: BridgeState,
}
#[cfg(feature = "broker-transport")]
impl InitialReadBridge {
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
        let deadline = crate::time::now() + crate::time::Duration::from_secs(3);
        loop {
            self.check()?;
            // Wait on the actual worker reply rather than polling and sleeping
            // the caller's thread. The bounded interval still revalidates
            // original custody and observes the construction deadline.
            match received.recv_timeout(crate::time::Duration::from_millis(10)) {
                Ok(result) => return result.map(|_| ()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("native initialization reply ended".into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if crate::time::now() >= deadline {
                return Err("native initialization commit timed out".into());
            }
        }
    }
    fn from_manager(manager: &CollaborationManager) -> Result<Option<Self>, String> {
        let Backend::Transport(transport) = &manager.backend else {
            return Ok(None);
        };
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
    fn apply_prepared_capture(
        &self,
        binding: &crate::bound_dispatch::CallbackBinding,
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
        read.with_mutation(|mutation| {
            binding
                .records
                .merge_remote_document(&binding.checkout, mutation.clone())
        })??;
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
    pub(crate) fn record(
        &self,
        app: &str,
        room: &str,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<CommittedDocument, String> {
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
            return Ok(read);
        }
        let read = if checkout.id.is_package_primary() {
            if let Some(initial) = primary(app, Some(room)) {
                initial.map(|(_, read)| read)?
            } else {
                self.read(None)?
            }
        } else {
            self.read(Some(checkout))?
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
        Ok(read)
    }
    fn read(
        &self,
        checkout: Option<&crate::record::RecordCheckout>,
    ) -> Result<CommittedDocument, String> {
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
        let deadline = crate::time::now() + crate::time::Duration::from_secs(3);
        loop {
            let seen = self.progress();
            self.check()?;
            if let Some(read) = self.state.document_reads.initial(&key) {
                return Ok(read);
            }
            if crate::time::now() >= deadline {
                return Err("native initial history unavailable before construction".into());
            }
            // The RPC worker has a separate runtime. No lock or application
            // document borrow crosses this bounded construction wait; staging
            // the capture ends it at once.
            self.wait_progress(seen, crate::time::Duration::from_millis(10));
        }
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
pub(crate) struct InitialReadBridge {
    never: std::convert::Infallible,
    route: RouteSpec,
}
#[cfg(not(feature = "broker-transport"))]
impl InitialReadBridge {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "mirrors the broker-backed bridge's signature"
    )]
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
    fn progress(&self) -> u64 {
        match self.never {}
    }
    fn wait_progress(&self, _seen: u64, _bound: crate::time::Duration) {
        match self.never {}
    }
    fn apply_prepared_capture(
        &self,
        _binding: &crate::bound_dispatch::CallbackBinding,
        _requested: &RouteSpec,
    ) -> Result<(), String> {
        match self.never {}
    }
    pub(crate) fn record(
        &self,
        _app: &str,
        _room: &str,
        _checkout: &crate::record::RecordCheckout,
    ) -> Result<CommittedDocument, String> {
        match self.never {}
    }
    fn read(&self, _record: Option<&crate::record::RecordId>) -> Result<CommittedDocument, String> {
        match self.never {}
    }
}

#[cfg(test)]
#[cfg(feature = "broker-transport")]
#[path = "native_initial/named_creation_tests.rs"]
mod named_creation_tests;

#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod tests {
    use super::{
        BridgeState, CollaborationManager, Construction, InitialReadBridge, NativeConstruction,
        RouteSpec, take_initialization,
    };
    use crate::collaboration::broker::{ByteBudget, committed_document::read_capture_using};
    use runtime_collab::committed_document::{
        CreationChunk, CreationReceipt, CreationWorkspace, Reply,
    };
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        rc::Rc,
    };

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
}
