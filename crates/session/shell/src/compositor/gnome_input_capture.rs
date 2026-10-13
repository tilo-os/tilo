//! Consented host input capture. Portal/EI correlation supplies input, never
//! identity or authority: the normal live-session and spatial-lease gates do.

use std::{collections::HashMap, os::unix::net::UnixStream, sync::Arc, time::Duration};

use ashpd::desktop::input_capture::{
    Barrier, BarrierID, BarrierPosition, Capabilities, ConnectToEISOptions, CreateSessionOptions,
    DisableOptions, EnableOptions, GetZonesOptions, InputCapture, ReleaseOptions,
    SetPointerBarriersOptions, Zones,
};
use futures_util::StreamExt;
use reis::{ei, event::EiEvent};
use tokio_util::sync::CancellationToken;

use super::native::{
    SpatialControlSession, end_spatial_control, move_observed_spatial_pointer_for_target,
    send_spatial_fabric_event, spatial_control_is_current, spatial_scroll,
};
use super::spatial_pointer::SpatialMotion;
use crate::shell::{
    collab_broker_capnp::{LocalSessionLifecycle, LocalSessionPermit},
    device_fabric::{FabricInputEvent, MAX_INPUT_LEASE_MS},
    device_fabric_runtime::person_layout,
    network::ShellNetwork,
};

/// Dropping the owner requests asynchronous session/held-input cleanup.
pub(crate) struct HostCapture {
    cancel: CancellationToken,
    _listener: Arc<dyn Fn() + Send + Sync>,
    worker: Option<std::thread::JoinHandle<()>>,
    observer: crate::shell::device_fabric_runtime::HostDisplayObserver,
}

impl HostCapture {
    pub(crate) fn display_observer(
        &self,
    ) -> crate::shell::device_fabric_runtime::HostDisplayObserver {
        Arc::clone(&self.observer)
    }
}

impl Drop for HostCapture {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Explicit opt-in only. This never unlocks, enrolls, grants or renews a lease.
pub(crate) fn start(
    network: Arc<ShellNetwork>,
    lifecycle: &LocalSessionLifecycle,
    target: &str,
) -> Result<HostCapture, String> {
    let target = crate::shell::personal_continuity::DeviceId::new(target.to_owned())
        .map_err(|error| error.to_string())?;
    let permit = lifecycle
        .acquire()
        .ok_or("host input requires an active unlocked owner session")?;
    let cancel = CancellationToken::new();
    let watched = permit.clone();
    let revoked = cancel.clone();
    let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if !watched.is_current() {
            revoked.cancel();
        }
    });
    lifecycle.watch(&listener);
    let task_cancel = cancel.clone();
    let geometry = super::gnome_display_observation::Geometry::new(cancel.clone());
    let observed_geometry = geometry.clone();
    let getter = geometry;
    let observer: crate::shell::device_fabric_runtime::HostDisplayObserver =
        Arc::new(move || getter.snapshot());
    // reis' passive converter retains non-Send protocol callbacks. Keep its
    // context and async lifecycle on one owned thread, like the existing EIS server.
    let worker = std::thread::Builder::new()
        .name("tilo-host-ei".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(%error, "host input runtime unavailable");
                    return;
                }
            };
            let result = runtime.block_on(async {
                let observer = super::gnome_display_observation::observe(observed_geometry.clone());
                let captured = async {
                    let result = capture(
                        network,
                        permit,
                        task_cancel.clone(),
                        (target, observed_geometry),
                    )
                    .await;
                    task_cancel.cancel();
                    result
                };
                let (captured, geometry_result) = tokio::join!(captured, observer);
                captured.and(geometry_result)
            });
            if let Err(error) = result {
                tracing::warn!(%error, "host input capture ended");
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(HostCapture {
        cancel,
        _listener: listener,
        worker: Some(worker),
        observer,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Activation {
    id: u32,
    barrier: u32,
    point: (f64, f64),
}

/// Correlate independent D-Bus/EI deliveries without queuing stale input.
struct CaptureEpoch<D = reis::event::Device> {
    portal: Option<Activation>,
    devices: HashMap<D, u32>,
}

impl<D> Default for CaptureEpoch<D> {
    fn default() -> Self {
        Self {
            portal: None,
            devices: HashMap::new(),
        }
    }
}

impl<D: std::hash::Hash + Eq> CaptureEpoch<D> {
    fn activate(&mut self, activation: Activation) {
        self.portal = Some(activation);
    }

    fn started(&mut self, device: D, id: u32) {
        self.devices.insert(device, id);
    }

    fn stopped(&mut self, device: &D) -> bool {
        let was_current = self.accepts(device);
        self.devices.remove(device);
        was_current
    }

    fn accepts(&self, device: &D) -> bool {
        self.portal
            .is_some_and(|active| self.devices.get(device) == Some(&active.id))
    }

    fn deactivate(&mut self, id: u32) -> bool {
        if self.portal.is_some_and(|active| active.id == id) {
            self.clear();
            true
        } else {
            false
        }
    }

    fn clear(&mut self) -> Option<Activation> {
        self.devices.clear();
        self.portal.take()
    }
}

struct CaptureBarrier {
    portal: Barrier,
    rect: settings_store::person_displays::Rect,
}

enum CaptureOutcome {
    Continue,
    Release(Option<(f64, f64)>),
}

struct SpatialRoute {
    network: Arc<ShellNetwork>,
    active: Option<SpatialControlSession>,
    barriers: Vec<CaptureBarrier>,
    target: crate::shell::personal_continuity::DeviceId,
    armed_layout: Option<Arc<person_layout::PersonLayoutView>>,
    armed_nonce: Option<[u8; 16]>,
    host_geometry: super::gnome_display_observation::Geometry,
}

impl Drop for SpatialRoute {
    fn drop(&mut self) {
        end_spatial_control(&self.network, &mut self.active);
    }
}

impl SpatialRoute {
    fn lease_is_current(&self) -> bool {
        self.armed_nonce.is_some_and(|nonce| {
            crate::shell::device_fabric_runtime::spatial_pointer_lease(&self.network, &self.target)
                .is_some_and(|lease| lease.nonce == nonce)
        })
    }

    fn geometry_is_current(&self) -> bool {
        self.host_geometry.snapshot().is_some()
            && self.armed_layout.as_ref().is_some_and(|armed| {
                person_layout::current(&self.network)
                    .is_some_and(|current| current.layout == armed.layout)
            })
    }

    fn enter(&mut self, activation: Activation) -> bool {
        let Some(barrier) = self
            .barriers
            .iter()
            .find(|item| item.portal.barrier_id().get() == activation.barrier)
        else {
            return false;
        };
        let rect = barrier.rect;
        let point = activation.point;
        if !point.0.is_finite()
            || !point.1.is_finite()
            || file_explorer::external_drag::unclaimed().is_some()
        {
            return false;
        }
        let anchor = (
            point
                .0
                .clamp(f64::from(rect.x), f64::from(rect.right()) - 0.001),
            point
                .1
                .clamp(f64::from(rect.y), f64::from(rect.bottom()) - 0.001),
        );
        matches!(
            move_observed_spatial_pointer_for_target(
                Some(anchor),
                &self.network,
                &mut self.active,
                (point.0 - anchor.0, point.1 - anchor.1),
                Some(&self.target)
            ),
            Some(SpatialMotion::Remote)
        )
    }

    fn motion(&mut self, delta: (f64, f64)) -> CaptureOutcome {
        if !delta.0.is_finite() || !delta.1.is_finite() {
            end_spatial_control(&self.network, &mut self.active);
            return CaptureOutcome::Release(None);
        }
        match move_observed_spatial_pointer_for_target(
            None,
            &self.network,
            &mut self.active,
            delta,
            Some(&self.target),
        ) {
            Some(SpatialMotion::Remote) => CaptureOutcome::Continue,
            Some(SpatialMotion::Returned(point)) => CaptureOutcome::Release(point),
            _ => CaptureOutcome::Release(None),
        }
    }

    fn send(&mut self, event: FabricInputEvent) -> bool {
        send_spatial_fabric_event(event, &self.network, &mut self.active)
    }

    fn end(&mut self) {
        end_spatial_control(&self.network, &mut self.active);
    }
}

/// Match actual portal zones to current authenticated connector geometry.
/// A headless/fake or stale layout mismatch is refused, never projected as hardware.
fn barriers_for_zones(
    network: &ShellNetwork,
    zones: &Zones,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(Arc<person_layout::PersonLayoutView>, Vec<CaptureBarrier>), String> {
    let view = person_layout::current(network).ok_or("no current certified connector layout")?;
    let regions: Vec<_> = zones
        .regions()
        .iter()
        .map(|region| settings_store::person_displays::Rect {
            x: region.x_offset(),
            y: region.y_offset(),
            width: region.width(),
            height: region.height(),
        })
        .collect();
    let barriers = barriers_for_layout(&view.layout, &regions, |display| {
        display.device == target.as_str() && person_layout::reachable(network, display)
    })?;
    Ok((view, barriers))
}

fn barriers_for_layout(
    layout: &settings_store::person_displays::Layout,
    zones: &[settings_store::person_displays::Rect],
    reachable: impl Fn(&settings_store::person_displays::ResolvedDisplay) -> bool,
) -> Result<Vec<CaptureBarrier>, String> {
    let mut result = Vec::new();
    for local in layout
        .displays
        .iter()
        .filter(|display| display.local && display.enabled)
    {
        let rect = local.local_rect;
        if !zones.contains(&rect) {
            return Err("portal zones differ from certified local connector geometry".into());
        }
        for remote in layout
            .displays
            .iter()
            .filter(|display| !display.local && reachable(display))
        {
            let a = local.rect;
            let b = remote.rect;
            let top = a.y.max(b.y);
            let bottom = a.bottom().min(b.bottom());
            let left = a.x.max(b.x);
            let right = a.right().min(b.right());
            let position = if top < bottom && (a.right() == b.x || a.x == b.right()) {
                let x = if a.right() == b.x {
                    rect.right()
                } else {
                    rect.x
                };
                let y = rect
                    .y
                    .checked_add(top.checked_sub(a.y).ok_or("capture offset overflow")?)
                    .ok_or("capture offset overflow")?;
                Some(BarrierPosition::new(
                    x,
                    y,
                    x,
                    y.checked_add(
                        bottom
                            .checked_sub(top)
                            .and_then(|length| length.checked_sub(1))
                            .ok_or("capture edge overflow")?,
                    )
                    .ok_or("capture edge overflow")?,
                ))
            } else if left < right && (a.bottom() == b.y || a.y == b.bottom()) {
                let y = if a.bottom() == b.y {
                    rect.bottom()
                } else {
                    rect.y
                };
                let x = rect
                    .x
                    .checked_add(left.checked_sub(a.x).ok_or("capture offset overflow")?)
                    .ok_or("capture offset overflow")?;
                Some(BarrierPosition::new(
                    x,
                    y,
                    x.checked_add(
                        right
                            .checked_sub(left)
                            .and_then(|length| length.checked_sub(1))
                            .ok_or("capture edge overflow")?,
                    )
                    .ok_or("capture edge overflow")?,
                    y,
                ))
            } else {
                None
            };
            if let Some(position) = position {
                let id = result
                    .len()
                    .checked_add(1)
                    .and_then(|count| u32::try_from(count).ok())
                    .and_then(BarrierID::new)
                    .ok_or("too many capture barriers")?;
                result.push(CaptureBarrier {
                    portal: Barrier::new(id, position),
                    rect,
                });
            }
        }
    }
    if result.is_empty() {
        return Err("no leased peer at an exposed local connector edge".into());
    }
    Ok(result)
}

#[allow(
    clippy::future_not_send,
    reason = "reis passive callbacks are thread-bound; this future runs only on the owned current-thread runtime"
)]
async fn capture(
    network: Arc<ShellNetwork>,
    permit: LocalSessionPermit,
    cancel: CancellationToken,
    host: (
        crate::shell::personal_continuity::DeviceId,
        super::gnome_display_observation::Geometry,
    ),
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(MAX_INPUT_LEASE_MS))
        .ok_or("capture deadline overflow")?;
    // Dedicated connection: cancelling a pending consent closes only our portal client.
    let portal = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        result = async {
            let connection = zbus::Connection::session().await.map_err(|error| error.to_string())?;
            InputCapture::with_connection(connection).await.map_err(|error| error.to_string())
        } => result?,
    };
    let setup = portal.create_session(
        None,
        CreateSessionOptions::default()
            .set_capabilities(Capabilities::Pointer | Capabilities::Keyboard),
    );
    let (session, capabilities) = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(Duration::from_secs(60), setup) => {
            result.map_err(|_| "host input consent timed out")?.map_err(|error| error.to_string())?
        }
    };
    let mut route = SpatialRoute {
        network,
        active: None,
        barriers: Vec::new(),
        target: host.0,
        host_geometry: host.1,
        armed_layout: None,
        armed_nonce: None,
    };
    let result = tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout_at(deadline,
            capture_session(&portal, &session, capabilities, (&permit, &cancel, deadline), &mut route)) => {
            result.map_err(|_| "bounded host capture window expired".to_owned()).and_then(std::convert::identity)
        }
    };
    route.end();
    drop(route);
    let cleanup = tokio::time::timeout(Duration::from_secs(3), async {
        let disabled = portal.disable(&session, DisableOptions::default()).await;
        let closed = session.close().await;
        disabled.and(closed).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "host capture cleanup timed out; closing dedicated connection".to_owned());
    result.and_then(|()| cleanup.and_then(std::convert::identity))
}

#[allow(
    clippy::future_not_send,
    reason = "reis passive callbacks are thread-bound; this future runs only on the owned current-thread runtime"
)]
async fn capture_session(
    portal: &InputCapture,
    session: &ashpd::desktop::Session<InputCapture>,
    capabilities: reis::enumflags2::BitFlags<Capabilities>,
    owner: (
        &LocalSessionPermit,
        &CancellationToken,
        tokio::time::Instant,
    ),
    route: &mut SpatialRoute,
) -> Result<(), String> {
    if !capabilities.contains(Capabilities::Pointer | Capabilities::Keyboard) {
        return Err("portal did not admit pointer and keyboard".into());
    }
    let path = serde_json::to_value(session)
        .map_err(|error| error.to_string())?
        .as_str()
        .ok_or("portal session is not an object path")?
        .to_owned();
    let mut closed = Box::pin(
        session
            .receive_closed()
            .await
            .map_err(|error| error.to_string())?,
    );
    // Subscribe before enabling: activation may precede the Enable method reply.
    let mut activated = Box::pin(
        portal
            .receive_activated()
            .await
            .map_err(|error| error.to_string())?,
    );
    let mut deactivated = Box::pin(
        portal
            .receive_deactivated()
            .await
            .map_err(|error| error.to_string())?,
    );
    let mut disabled = Box::pin(
        portal
            .receive_disabled()
            .await
            .map_err(|error| error.to_string())?,
    );
    let mut changed = Box::pin(
        portal
            .receive_zones_changed()
            .await
            .map_err(|error| error.to_string())?,
    );
    let (context, mut events) = prepare_capture(portal, session, owner.0, route).await?;
    let mut epoch = CaptureEpoch::default();
    let deadline = tokio::time::sleep_until(owner.2);
    tokio::pin!(deadline);
    let mut recheck = tokio::time::interval(Duration::from_millis(250));
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let return_point = tokio::select! {
            () = owner.1.cancelled() => break,
            () = &mut deadline => break,
            _ = closed.next() => break,
            _ = recheck.tick() => {
                if !owner.0.is_current()
                    || !route.geometry_is_current()
                    || !route.lease_is_current()
                    || (route.active.is_some() && !spatial_control_is_current(&route.network, route.active.as_ref()))
                { break; }
                CaptureOutcome::Continue
            },
            event = changed.next() => {
                if event.is_none_or(|event| event.session_handle().as_str() == path) { break; }
                CaptureOutcome::Continue
            }
            event = disabled.next() => {
                if event.is_none_or(|event| event.session_handle().as_str() == path) { break; }
                CaptureOutcome::Continue
            }
            event = activated.next() => {
                let Some(event) = event else { break; };
                if event.session_handle().as_str() != path { continue; }
                route.end();
                let id = event.activation_id().ok_or("activation missing its EI correlation ID")?;
                let point = event.cursor_position().ok_or("activation missing cursor position")?;
                let barrier = match event.barrier_id() {
                    Some(ashpd::desktop::input_capture::ActivatedBarrier::Barrier(id)) => id.get(),
                    _ => return Err("activation does not name our barrier".into()),
                };
                epoch.activate(Activation { id, barrier, point: (f64::from(point.0), f64::from(point.1)) });
                CaptureOutcome::Continue
            }
            event = deactivated.next() => {
                let Some(event) = event else { break; };
                if event.session_handle().as_str() == path
                    && event.activation_id().is_some_and(|id| epoch.deactivate(id)) { route.end(); }
                CaptureOutcome::Continue
            }
            event = events.next() => {
                let Some(event) = event else { break; };
                let event = event.map_err(|error| error.to_string())?;
                dispatch_ei(event, &context, &mut epoch, route, owner.0)?
            }
        };
        if let CaptureOutcome::Release(point) = return_point {
            route.end();
            release_capture(portal, session, &mut epoch, point).await?;
        }
    }
    Ok(())
}

async fn release_capture(
    portal: &InputCapture,
    session: &ashpd::desktop::Session<InputCapture>,
    epoch: &mut CaptureEpoch,
    point: Option<(f64, f64)>,
) -> Result<(), String> {
    if let Some(active) = epoch.clear() {
        portal
            .release(
                session,
                ReleaseOptions::default()
                    .set_activation_id(active.id)
                    .set_cursor_position(point),
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[allow(
    clippy::future_not_send,
    reason = "reis passive callbacks are thread-bound; this future runs only on the owned current-thread runtime"
)]
async fn prepare_capture(
    portal: &InputCapture,
    session: &ashpd::desktop::Session<InputCapture>,
    permit: &LocalSessionPermit,
    route: &mut SpatialRoute,
) -> Result<
    (
        ei::Context,
        std::pin::Pin<Box<reis::tokio::EiConvertEventStream>>,
    ),
    String,
> {
    let fd = portal
        .connect_to_eis(session, ConnectToEISOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let context = ei::Context::new(stream).map_err(|error| error.to_string())?;
    let (_connection, events) = context
        .handshake_tokio("Tilo host input", ei::handshake::ContextType::Receiver)
        .await
        .map_err(|error| error.to_string())?;
    let mut events = Box::pin(events);
    tokio::time::timeout(
        Duration::from_secs(5),
        prepare_ei_devices(&mut events, &context),
    )
    .await
    .map_err(|_| "host EI device negotiation timed out")??;
    let zones = portal
        .zones(session, GetZonesOptions::default())
        .await
        .map_err(|error| error.to_string())?
        .response()
        .map_err(|error| error.to_string())?;
    route.armed_nonce = Some(
        crate::shell::device_fabric_runtime::spatial_pointer_lease(&route.network, &route.target)
            .ok_or("target lease expired during capture setup")?
            .nonce,
    );
    let (layout, barriers) = wait_for_host_layout(route, &zones).await?;
    route.armed_layout = Some(layout);
    route.barriers = barriers;
    let requested: Vec<_> = route
        .barriers
        .iter()
        .map(|barrier| Barrier::new(barrier.portal.barrier_id(), barrier.portal.position()))
        .collect();
    let response = portal
        .set_pointer_barriers(
            session,
            &requested,
            zones.zone_set(),
            SetPointerBarriersOptions::default(),
        )
        .await
        .map_err(|error| error.to_string())?
        .response()
        .map_err(|error| error.to_string())?;
    route.barriers.retain(|barrier| {
        !response
            .failed_barriers()
            .contains(&barrier.portal.barrier_id())
    });
    if route.barriers.is_empty() {
        return Err("portal refused all leased display edge barriers".into());
    }
    if !permit.is_current() || !route.lease_is_current() {
        return Err("owner session or target lease changed during capture setup".into());
    }
    portal
        .enable(session, EnableOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    Ok((context, events))
}

async fn wait_for_host_layout(
    route: &SpatialRoute,
    zones: &Zones,
) -> Result<(Arc<person_layout::PersonLayoutView>, Vec<CaptureBarrier>), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if route.host_geometry.snapshot().is_none() || !route.lease_is_current() {
                return Err("live GNOME geometry or original target lease unavailable".into());
            }
            if let Ok(layout) = barriers_for_zones(&route.network, zones, &route.target) {
                return Ok(layout);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "certified GNOME connector layout did not converge".to_owned())?
}

async fn prepare_ei_devices<S, E>(events: &mut S, context: &ei::Context) -> Result<(), String>
where
    S: futures_util::Stream<Item = Result<EiEvent, E>> + Unpin,
    E: std::fmt::Display,
{
    use reis::event::DeviceCapability;
    let mut pointer = false;
    let mut keyboard = false;
    while !(pointer && keyboard) {
        match events
            .next()
            .await
            .ok_or("host EI closed during device negotiation")?
            .map_err(|error| error.to_string())?
        {
            EiEvent::SeatAdded(event) => {
                event.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                context.flush().map_err(|error| error.to_string())?;
            }
            EiEvent::DeviceAdded(event) => {
                pointer |= event.device.has_capability(DeviceCapability::Pointer);
                keyboard |= event.device.has_capability(DeviceCapability::Keyboard);
            }
            EiEvent::Disconnected(_) | EiEvent::SeatRemoved(_) => {
                return Err("host EI disconnected during negotiation".into());
            }
            EiEvent::DeviceStartEmulating(_) => {
                return Err("host EI started capture before portal was enabled".into());
            }
            _ => {}
        }
    }
    Ok(())
}

fn dispatch_ei(
    event: EiEvent,
    context: &ei::Context,
    epoch: &mut CaptureEpoch,
    route: &mut SpatialRoute,
    permit: &LocalSessionPermit,
) -> Result<CaptureOutcome, String> {
    use reis::event::DeviceCapability;
    match &event {
        EiEvent::SeatAdded(event) => {
            event.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll,
            );
            context.flush().map_err(|error| error.to_string())?;
            return Ok(CaptureOutcome::Continue);
        }
        EiEvent::Disconnected(_) | EiEvent::SeatRemoved(_) => {
            return Err("host EI disconnected".into());
        }
        EiEvent::DeviceStartEmulating(event) => {
            epoch.started(event.device.clone(), event.sequence);
        }
        EiEvent::DeviceStopEmulating(event) => {
            let was_current = epoch.stopped(&event.device);
            return Ok(if was_current {
                CaptureOutcome::Release(None)
            } else {
                CaptureOutcome::Continue
            });
        }
        EiEvent::DevicePaused(_) | EiEvent::DeviceRemoved(_) => {
            return Err("captured EI device became unavailable".into());
        }
        _ => {}
    }
    let Some(device) = event.device() else {
        return Ok(CaptureOutcome::Continue);
    };
    if !epoch.accepts(device) {
        return Ok(CaptureOutcome::Continue);
    }
    permit
        .while_current(|| {
            if !route.geometry_is_current() || !route.lease_is_current() {
                return Err("certified capture geometry or original target lease changed".into());
            }
            if route.active.is_none() && !epoch.portal.is_some_and(|active| route.enter(active)) {
                return Ok(CaptureOutcome::Release(None));
            }
            let input = match event {
                EiEvent::PointerMotion(event) => {
                    return Ok(route.motion((f64::from(event.dx), f64::from(event.dy))));
                }
                EiEvent::KeyboardKey(event) => {
                    if event.key == 1 && event.state == ei::keyboard::KeyState::Press {
                        return Ok(CaptureOutcome::Release(None));
                    }
                    Some(FabricInputEvent::Key {
                        keycode: event.key,
                        pressed: event.state == ei::keyboard::KeyState::Press,
                        modifiers: 0,
                    })
                }
                EiEvent::Button(event) => Some(FabricInputEvent::PointerButton {
                    button: event.button,
                    pressed: event.state == ei::button::ButtonState::Press,
                }),
                EiEvent::ScrollDelta(event) if event.dx.is_finite() && event.dy.is_finite() => {
                    Some(FabricInputEvent::Scroll {
                        horizontal: spatial_scroll(f64::from(event.dx)),
                        vertical: spatial_scroll(f64::from(event.dy)),
                    })
                }
                EiEvent::ScrollDiscrete(event) => Some(FabricInputEvent::Scroll {
                    horizontal: spatial_scroll(f64::from(event.discrete_dx) / 120.0),
                    vertical: spatial_scroll(f64::from(event.discrete_dy) / 120.0),
                }),
                EiEvent::ScrollDelta(_) => return Err("non-finite captured scroll".into()),
                _ => None,
            };
            if input.is_some_and(|input| !route.send(input)) {
                return Err("existing spatial input gate refused host input".into());
            }
            Ok(CaptureOutcome::Continue)
        })
        .ok_or_else(|| "normal local session permit expired".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::{Activation, CaptureEpoch, barriers_for_layout};
    use crate::shell::collab_broker_capnp::LocalSessionAuthority;
    use settings_store::person_displays::{Arrangement, DeviceOutput, Layout, PersonDevice, Rect};

    fn activation(id: u32) -> Activation {
        Activation {
            id,
            barrier: 1,
            point: (1445.0, 450.0),
        }
    }

    #[test]
    fn independent_portal_and_ei_delivery_never_admits_uncorrelated_input() {
        for portal_first in [false, true] {
            let mut epoch = CaptureEpoch::<u32>::default();
            assert!(!epoch.accepts(&1)); // Enable is not activation.
            if portal_first {
                epoch.activate(activation(7));
            } else {
                epoch.started(1, 7);
            }
            assert!(!epoch.accepts(&1));
            if portal_first {
                epoch.started(1, 7);
            } else {
                epoch.activate(activation(7));
            }
            assert!(epoch.accepts(&1));
            epoch.started(2, 6);
            assert!(!epoch.accepts(&2)); // A second device still belongs to an old capture.
            epoch.started(2, 7);
            assert!(epoch.accepts(&2));
        }
    }

    #[test]
    fn delayed_deactivation_does_not_release_a_new_capture_even_across_id_wrap() {
        let mut epoch = CaptureEpoch::<u32>::default();
        epoch.activate(activation(u32::MAX));
        epoch.started(1, u32::MAX);
        assert!(epoch.accepts(&1));
        epoch.activate(activation(0));
        assert!(!epoch.accepts(&1));
        epoch.started(1, 0);
        assert!(!epoch.deactivate(u32::MAX));
        assert!(epoch.accepts(&1));
        assert!(epoch.deactivate(0));
        assert!(!epoch.accepts(&1));
    }

    #[test]
    fn device_stop_and_own_shutdown_do_not_wait_for_a_portal_deactivation() {
        let mut epoch = CaptureEpoch::<u32>::default();
        epoch.activate(activation(8));
        epoch.started(1, 8);
        epoch.started(2, 7);
        assert!(!epoch.stopped(&2));
        assert!(epoch.accepts(&1));
        assert!(epoch.stopped(&1));
        assert!(!epoch.accepts(&1));
        assert_eq!(epoch.clear(), Some(activation(8)));
        epoch.started(1, 8); // Delayed EI input cannot restore portal authority.
        assert!(!epoch.accepts(&1));
        assert_eq!(epoch.clear(), None);
    }

    #[test]
    fn normal_session_lock_invalidates_the_capture_permit_and_prevents_route_effects() {
        let authority = LocalSessionAuthority::new();
        let lifecycle = authority.lifecycle();
        assert!(lifecycle.acquire().is_none());
        lifecycle.publish_native_state(false, false);
        let permit = lifecycle
            .acquire()
            .expect("ordinary unlocked owner session");
        let mut routed = 0;
        assert!(permit.while_current(|| routed += 1).is_some());
        lifecycle.set_locked(true);
        assert!(permit.while_current(|| routed += 1).is_none());
        lifecycle.set_locked(false);
        assert!(permit.while_current(|| routed += 1).is_none()); // No automatic revival after unlock.
        assert_eq!(routed, 1);
    }

    fn output(id: &str, width: u32, height: u32) -> DeviceOutput {
        DeviceOutput {
            id: id.into(),
            name: id.into(),
            builtin: false,
            x: 0,
            y: 0,
            width,
            height,
            mode_width: width,
            mode_height: height,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        }
    }

    fn layout() -> Layout {
        Layout::resolve(
            &[
                PersonDevice {
                    device: "laptop".into(),
                    label: "Laptop".into(),
                    local: true,
                    online: true,
                    outputs: vec![output("panel", 1440, 900)],
                },
                PersonDevice {
                    device: "pi".into(),
                    label: "Pi".into(),
                    local: false,
                    online: true,
                    outputs: vec![output("hdmi", 1920, 1080)],
                },
            ],
            &Arrangement::default(),
        )
    }

    #[test]
    fn portal_geometry_and_the_existing_spatial_router_agree_on_overshoot_and_return() {
        use crate::compositor::spatial_pointer::{MotionPlan, PointerAt, plan_motion};
        let layout = layout();
        let zones = [Rect {
            x: 0,
            y: 0,
            width: 1440,
            height: 900,
        }];
        let barriers = barriers_for_layout(&layout, &zones, |_| true).expect("actual source zone");
        assert_eq!(barriers.len(), 1);
        assert_eq!(
            barriers[0].portal.position(),
            ashpd::desktop::input_capture::BarrierPosition::new(1440, 0, 1440, 899)
        );
        let crossed = plan_motion(
            &layout,
            "laptop",
            PointerAt::Local((1439.0, 450.0)),
            (6.0, 0.0),
            |_| true,
        );
        assert!(matches!(
            crossed,
            MotionPlan::Remote {
                device_point: (5.0, 450.0),
                ..
            }
        ));
        let returned = plan_motion(
            &layout,
            "laptop",
            PointerAt::Remote {
                device: "pi",
                output: "hdmi",
                shared: (1445.0, 450.0),
            },
            (-10.0, 0.0),
            |_| true,
        );
        assert_eq!(
            returned,
            MotionPlan::Return {
                local_point: Some((1435.0, 450.0))
            }
        );
    }

    #[test]
    fn a_fake_headless_zone_or_unleased_peer_cannot_arm_a_capture_barrier() {
        let layout = layout();
        let wrong = [Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        assert!(barriers_for_layout(&layout, &wrong, |_| true).is_err());
        let actual = [Rect {
            x: 0,
            y: 0,
            width: 1440,
            height: 900,
        }];
        assert!(barriers_for_layout(&layout, &actual, |_| false).is_err());
    }

    #[test]
    fn passive_ei_receiver_negotiates_actual_devices_before_capture_enable() {
        use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
        use reis::request::{DeviceCapability, EisRequest};
        use smithay::reexports::calloop::{EventLoop, PostAction, channel};
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let (server, client) = UnixStream::pair().expect("owned EI transport");
        let (stop, stopped) = channel::channel();
        let worker = std::thread::spawn(move || {
            let context = reis::eis::Context::new(server).expect("fixture EIS");
            let mut event_loop = EventLoop::<bool>::try_new().expect("fixture loop");
            event_loop
                .handle()
                .insert_source(
                    EisRequestSource::new(context, 1),
                    |event, connection, running| {
                        match event {
                            Ok(EisRequestSourceEvent::Connected) => {
                                let _seat = connection.add_seat(
                                    Some("synthetic capture fixture"),
                                    DeviceCapability::Pointer | DeviceCapability::Keyboard,
                                );
                            }
                            Ok(EisRequestSourceEvent::Request(EisRequest::Bind(bind))) => {
                                let device = bind.seat.add_device(
                                    Some("synthetic pointer and keyboard"),
                                    reis::eis::device::DeviceType::Virtual,
                                    bind.capabilities,
                                    |_| {},
                                );
                                device.resumed();
                            }
                            Err(_) | Ok(EisRequestSourceEvent::Request(EisRequest::Disconnect)) => {
                                *running = false;
                                return Ok(PostAction::Remove);
                            }
                            _ => {}
                        }
                        connection.flush().expect("fixture flush");
                        Ok(PostAction::Continue)
                    },
                )
                .expect("owned EI source");
            event_loop
                .handle()
                .insert_source(stopped, |_, (), running| *running = false)
                .expect("fixture stop source");
            let mut running = true;
            while running {
                event_loop
                    .dispatch(Duration::from_millis(50), &mut running)
                    .expect("fixture dispatch");
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let result = runtime.block_on(async {
            client.set_nonblocking(true).expect("receiver nonblocking");
            let context = reis::ei::Context::new(client).expect("passive receiver");
            let (_connection, stream) = context
                .handshake_tokio(
                    "synthetic host capture fixture",
                    reis::ei::handshake::ContextType::Receiver,
                )
                .await
                .expect("actual EI handshake");
            let mut stream = Box::pin(stream);
            tokio::time::timeout(
                Duration::from_secs(5),
                super::prepare_ei_devices(&mut stream, &context),
            )
            .await
        });
        let _ = stop.send(());
        worker.join().expect("own fixture joined");
        result
            .expect("bounded device negotiation")
            .expect("normal EI devices ready without enabling capture");
    }
}
