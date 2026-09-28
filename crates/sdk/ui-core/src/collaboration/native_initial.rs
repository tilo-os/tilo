//! Private native construction custody. It borrows the existing SDK worker;
//! neither an environment value nor a data capture is a positive authority.
#![cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "construction custody is driven by the native launcher; portable hosts use only the initial-read bridge"
    )
)]
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
    failure: RefCell<Option<String>>,
    transferring: Cell<bool>,
    component_scope: RefCell<Option<crate::wake::WakeScope>>,
}
pub(crate) struct NativeConstruction {
    state: Rc<Construction>,
    previous: Option<Weak<Construction>>,
}
impl NativeConstruction {
    pub(crate) fn enter_prebuilt(manager: CollaborationManager) -> Result<Self, String> {
        if InitialReadBridge::from_manager(&manager)?.is_some() {
            return Err("native Required history must be read before building the root".into());
        }
        Self::enter(manager)
    }
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
        let state = Rc::new(Construction {
            manager: RefCell::new(Some(manager)),
            bridge,
            primary: RefCell::new(primary),
            failure: RefCell::new(None),
            transferring: Cell::new(false),
            component_scope: RefCell::new(scope),
        });
        let previous = CONSTRUCTION.with(|slot| slot.replace(Some(Rc::downgrade(&state))));
        Ok(Self { state, previous })
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "native component admission briefly waits on the broker worker's owner recheck, as the broker transport module does"
    )]
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
                bridge.check()?;
                if bridge.publishable(&requested.channel_id()) {
                    break;
                }
                if crate::time::now() >= deadline {
                    return Err(
                        "native component history was not confirmed before its factory".into(),
                    );
                }
                std::thread::sleep(crate::time::Duration::from_millis(10));
            }
        }
        manager.switch_native_record(&binding.checkout)?;
        if manager
            .primary_route()
            .is_none_or(|witness| binding.admission_route().as_ref() != Some(&witness.route))
        {
            return Err("component route changed during native admission".into());
        }
        Ok(())
    }
    pub(crate) fn state_opened(&self) -> Result<(), String> {
        self.check()?;
        if self.state.primary.borrow().is_some() {
            return Err("native state opener did not consume the required initial history".into());
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
pub(crate) fn file_bridge() -> Result<Option<InitialReadBridge>, String> {
    if let Some(state) = current() {
        if let Some(error) = state.failure.borrow().clone() {
            return Err(error);
        }
        if let Some(bridge) = &state.bridge {
            bridge.check()?;
        }
        return Ok(state.bridge.clone());
    }
    if crate::host::environment()
        .identity
        .collab_service_configured
    {
        return Err("file record mode must be resolved by the native launch owner".into());
    }
    Ok(None)
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
        if checkout.id.is_package_primary() {
            if let Some(initial) = primary(app, Some(room)) {
                return initial.map(|(_, read)| read);
            }
            return self.read(None);
        }
        self.read(Some(checkout))
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
            self.check()?;
            if let Some(read) = self.state.document_reads.initial(&key) {
                return Ok(read);
            }
            if crate::time::now() >= deadline {
                return Err("native initial history unavailable before construction".into());
            }
            // The RPC worker has a separate runtime. No lock or application
            // document borrow crosses this bounded construction wait.
            std::thread::sleep(crate::time::Duration::from_millis(10));
        }
    }
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
