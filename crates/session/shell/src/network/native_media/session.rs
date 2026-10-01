use super::{
    Arc, AtomicBool, AtomicU64, CallTarget, Client, Command, Duration, Instant, Mutex,
    NativeWorkspaceOwner, OPEN_DEADLINE, Operation, Ordering, Reply, Ticket, Weak, authority,
    devices, observer,
};
use crate::{
    network::NetCmd,
    portal::{CaptureBroker, capture_broker::BorrowedCaptureHandle},
    shell::native_media::{MediaFactory, MediaSession, Prepare},
};
use media::voice::{
    self,
    gst::{CaptureSource, CaptureStop, ObservedOutput, VoiceAudio},
};
use runtime_collab::native_media::{Inventory, Issue, State as CallState};
use std::os::fd::AsFd;
use tilo_moq::client::{self, Status};
use tilo_relay_protocol::voice::HEARTBEAT;
use tokio::sync::mpsc;

pub(crate) struct Factory {
    commands: mpsc::Sender<NetCmd>,
    owner: NativeWorkspaceOwner,
    devices: Arc<devices::Devices>,
    runtime: tokio::runtime::Handle,
}
impl Factory {
    pub(crate) fn new(
        commands: mpsc::Sender<NetCmd>,
        owner: NativeWorkspaceOwner,
        broker: Arc<std::sync::Mutex<CaptureBroker>>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            commands,
            owner,
            devices: devices::Devices::start(broker, &runtime),
            runtime,
        }
    }
}
impl MediaFactory for Factory {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`runtime` is moved into the returned session after the maintenance task and \
                  its abort handle are wired up; there is no earlier point to drop it"
    )]
    fn prepare(&self, target: CallTarget) -> Prepare {
        let commands = self.commands.clone();
        let owner = self.owner.clone();
        let devices = self.devices.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let mut preparing = AbortTask(runtime.spawn(async move {
                let observed = devices.prepare().await.map_err(|_| Issue::Unavailable)?;
                let output = devices.output().ok_or(Issue::Unavailable)?;
                if !observed.contains(output.cookie, output.node, output.serial, false) {
                    return Err(Issue::SourceLost);
                }
                let audio = VoiceAudio::new(ObservedOutput {
                    node: output.node,
                    serial: output.serial,
                    remote_fd: observed.socket.as_fd(),
                })
                .map_err(|_| Issue::Unavailable)?;
                // VoiceAudio retains its own descriptor for this exact socket.
                // The observer has already relinquished every protocol reader.
                drop(observed);
                let opened = command(&commands, Operation::Open { owner, target })
                    .await
                    .map_err(|_| Issue::Denied)?;
                let Reply::Opened(ticket) = opened else {
                    return Err(Issue::Unavailable);
                };
                if ticket.granted(OPEN_DEADLINE).await.is_err() {
                    ticket.stop();
                    return Err(Issue::Denied);
                }
                let runtime = Arc::new(Running {
                    ticket: ticket.clone(),
                    client: Mutex::new(None),
                    state: Mutex::new(Ok(CallState::Joining)),
                    cancelled: AtomicBool::new(false),
                    task: Mutex::new(None),
                    commands: commands.clone(),
                    executor: tokio::runtime::Handle::current(),
                });
                let controller = runtime.clone();
                let task = tokio::spawn(async move {
                    let result = maintain(&commands, &controller).await;
                    if !controller.cancelled.load(Ordering::Acquire) {
                        controller.ticket.stop();
                        *controller.state.lock() = result
                            .map(|()| CallState::Ended)
                            .map_err(|_| Issue::TransportFailed);
                    }
                });
                *runtime.task.lock() = Some(task.abort_handle());
                Ok(Box::new(Session {
                    runtime,
                    devices,
                    output,
                    audio,
                    capture: None,
                    decoded_generation: None,
                    publishers: Vec::new(),
                }) as Box<dyn MediaSession>)
            }));
            (&mut preparing.0).await.map_err(|_| Issue::Unavailable)?
        })
    }
}
struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Running {
    ticket: Arc<Ticket>,
    client: Mutex<Option<Arc<Client>>>,
    state: Mutex<Result<CallState, Issue>>,
    cancelled: AtomicBool,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    commands: mpsc::Sender<NetCmd>,
    executor: tokio::runtime::Handle,
}
impl Running {
    fn retire(&self) {
        self.ticket.clear_carrier();
        let client = self.client.lock().take();
        if let Some(client) = client {
            client.close();
        }
        *self.state.lock() = Ok(CallState::Reconnecting);
    }
}
async fn command(commands: &mpsc::Sender<NetCmd>, operation: Operation) -> Result<Reply, String> {
    let (reply, receive) = tokio::sync::oneshot::channel();
    tokio::time::timeout(Duration::from_secs(3), async {
        commands
            .send(NetCmd::NativeMedia(Command { operation, reply }))
            .await
            .map_err(|_| "native actor stopped".to_string())?;
        receive
            .await
            .map_err(|_| "native actor reply lost".to_string())?
    })
    .await
    .map_err(|_| "native actor deadline".to_string())?
}
/// Start a carrier for the current grant; `false` when it admits no route.
fn install_carrier(running: &Arc<Running>) -> Result<bool, String> {
    let ticket = &running.ticket;
    let Some(connection) = ticket.next_carrier()? else {
        return Ok(false);
    };
    if running.cancelled.load(Ordering::Acquire) {
        return Err("native session closed".into());
    }
    let client =
        Arc::new(Client::start(connection, ticket.clone()).map_err(|error| error.to_string())?);
    ticket.set_client(&client)?;
    *running.client.lock() = Some(client);
    Ok(true)
}
/// `Instant::now()` plus `delay`, saturating at now for an unrepresentable deadline.
fn later(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}
async fn maintain(commands: &mpsc::Sender<NetCmd>, running: &Arc<Running>) -> Result<(), String> {
    let ticket = &running.ticket;
    let mut heartbeat = later(HEARTBEAT);
    let mut failures = 0_u32;
    let mut live_since = None;
    let mut retry = Instant::now();
    loop {
        let changed = ticket.changed().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if running.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        ticket.live()?;
        if !ticket.authorized() {
            return Err("native participation lapsed".into());
        }
        let now = Instant::now();
        // A new grant with another scope (roster, directions) is followed by
        // a new carrier; that is no failure.
        if ticket.stale() {
            live_since = None;
            running.retire();
            retry = now;
        }
        let status = running.client.lock().as_ref().map(|client| client.status());
        match status {
            Some(Status::Live) => {
                *running.state.lock() = Ok(CallState::Live);
                if now.duration_since(*live_since.get_or_insert(now)) >= Duration::from_secs(5) {
                    failures = 0;
                }
            }
            Some(Status::Connecting) => {
                live_since = None;
            }
            Some(Status::Failed | Status::Denied | Status::Closed) => {
                live_since = None;
                running.retire();
                failures = failures.saturating_add(1);
                if failures >= 3 {
                    return Err("native carrier retry budget exhausted".into());
                }
                retry = later(Duration::from_millis(200));
            }
            None if now >= retry => {
                if install_carrier(running).is_err() {
                    running.retire();
                    failures = failures.saturating_add(1);
                    if failures >= 3 {
                        return Err("native carrier retry budget exhausted".into());
                    }
                    retry = later(Duration::from_millis(200 << failures));
                }
            }
            None => {}
        }
        if now >= heartbeat {
            command(
                commands,
                Operation::Heartbeat {
                    ticket: ticket.clone(),
                },
            )
            .await?;
            heartbeat = later(HEARTBEAT);
        }
        tokio::select! {
            () = changed => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

struct StopSlot {
    current: AtomicBool,
    startup: AtomicU64,
    ready: AtomicBool,
    generation: Mutex<Option<(CaptureStop, Weak<Client>)>>,
}
impl StopSlot {
    fn new() -> Self {
        Self {
            current: AtomicBool::new(true),
            startup: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            generation: Mutex::new(None),
        }
    }
    fn begin(&self) -> Result<u64, Issue> {
        let _generation = self.generation.lock();
        if !self.current.load(Ordering::SeqCst) {
            return Err(Issue::PermissionDenied);
        }
        self.advance()
    }
    // Caller holds generation while changing startup or checking its deadline.
    fn advance(&self) -> Result<u64, Issue> {
        self.ready.store(false, Ordering::SeqCst);
        self.startup
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Issue::CaptureFailed)
            .and_then(|value| value.checked_add(1).ok_or(Issue::CaptureFailed))
    }
    fn pause(&self) {
        let mut generation = self.generation.lock();
        // A timer or prepared FD from the previous start cannot affect a later
        // resume of the same explicit unmute intent.
        if self.advance().is_err() {
            self.current.store(false, Ordering::SeqCst);
        }
        if let Some((capture, client)) = generation.take() {
            if let Some(client) = client.upgrade() {
                client.stop_publication();
            }
            capture.stop();
        }
    }
    fn stop(&self) {
        self.current.store(false, Ordering::SeqCst);
        self.pause();
    }
    fn expire(&self, startup: u64) {
        let mut generation = self.generation.lock();
        if self.startup.load(Ordering::SeqCst) == startup && !self.ready.load(Ordering::SeqCst) {
            self.current.store(false, Ordering::SeqCst);
            let _ = self.advance();
            if let Some((capture, client)) = generation.take() {
                if let Some(client) = client.upgrade() {
                    client.stop_publication();
                }
                capture.stop();
            }
        }
    }
}
struct PendingCapture {
    receive: tokio::sync::oneshot::Receiver<Result<observer::Observed, String>>,
    task: tokio::task::AbortHandle,
}
impl Drop for PendingCapture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct CaptureIntent {
    source_id: String,
    source: Arc<devices::Observation>,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    slot: Arc<StopSlot>,
    startup: u64,
    pending: Option<PendingCapture>,
    prepared: Option<observer::Observed>,
    watchdog: Option<tokio::task::AbortHandle>,
}
impl CaptureIntent {
    fn cancel_pending(&mut self) {
        self.pending = None;
        self.prepared = None;
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.abort();
        }
    }
}
impl Drop for CaptureIntent {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}
struct Session {
    runtime: Arc<Running>,
    devices: Arc<devices::Devices>,
    output: Arc<devices::Observation>,
    audio: VoiceAudio,
    capture: Option<CaptureIntent>,
    decoded_generation: Option<u64>,
    publishers: Vec<voice::PublicationId>,
}
impl Session {
    fn install_prepared_capture(&mut self, client: &Arc<Client>) -> Result<(), Issue> {
        let Some(intent) = &mut self.capture else {
            return Ok(());
        };
        if let Some(pending) = &mut intent.pending {
            match pending.receive.try_recv() {
                Ok(Ok(observed)) => {
                    intent.prepared = Some(observed);
                    intent.pending = None;
                }
                Ok(Err(_)) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    return Err(Issue::SourceLost);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return Ok(()),
            }
        }
        let Some(observed) = &intent.prepared else {
            return Ok(());
        };
        if !observed.contains(
            intent.source.cookie,
            intent.source.node,
            intent.source.serial,
            true,
        ) {
            return Err(Issue::SourceLost);
        }
        let startup = intent.startup;
        let slot = intent.slot.clone();
        let source = intent.source.clone();
        let allowed = intent.allowed.clone();
        let weak_slot = Arc::downgrade(&slot);
        let ticket = Arc::downgrade(&self.runtime.ticket);
        let current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            let (Some(slot), Some(ticket)) = (weak_slot.upgrade(), ticket.upgrade()) else {
                return false;
            };
            slot.current.load(Ordering::SeqCst)
                && slot.startup.load(Ordering::SeqCst) == startup
                && allowed()
                && source.current()
                && ticket.live().is_ok()
                && !ticket.leaving()
                && ticket.authorized()
        });
        // Fresh owner reads precede the independent slot lock. Owner fences
        // may hold their publication gate while synchronously stopping a slot.
        client
            .enable_publication()
            .map_err(|_| Issue::PermissionDenied)?;
        let mut installing = slot.generation.lock();
        if !current() {
            client.stop_publication();
            return Err(Issue::PermissionDenied);
        }
        let (node, serial) = observed.call_capture_node(intent.source.node, intent.source.serial);
        match self.audio.start_capture(
            CaptureSource::PipeWire {
                node,
                serial,
                remote_fd: observed.socket.as_fd(),
            },
            current,
        ) {
            Ok(()) => {}
            Err(voice::AudioError::Capacity) => {
                // Capacity is checked before any worker takes this socket.
                // Keep it until the original two-second startup deadline.
                client.stop_publication();
                return Ok(());
            }
            Err(_) => {
                client.stop_publication();
                return Err(Issue::CaptureFailed);
            }
        }
        let Some(stop) = self.audio.capture_stop_handle() else {
            self.audio.stop_capture();
            client.stop_publication();
            return Err(Issue::CaptureFailed);
        };
        *installing = Some((stop, Arc::downgrade(client)));
        drop(installing);
        intent.prepared = None;
        Ok(())
    }
    fn retire_decoders(&mut self) {
        for publisher in self.publishers.drain(..) {
            self.audio.remove_publisher(&publisher);
        }
        self.decoded_generation = None;
    }
}
impl MediaSession for Session {
    #[allow(
        clippy::too_many_lines,
        reason = "one call tick: carrier status, decoder generation, capture install, bounded \
                  send and receive; splitting it scatters the ordering the comments rely on"
    )]
    fn tick(&mut self, now: Instant) -> Result<CallState, Issue> {
        if !self.output.current() {
            self.stop_capture();
            return Err(Issue::SourceLost);
        }
        self.runtime.ticket.live().map_err(|_| Issue::Denied)?;
        self.audio.playout(now).map_err(|_| Issue::Unavailable)?;
        if !self.audio.output_ready() {
            return Ok(CallState::Joining);
        }
        let status = *self.runtime.state.lock();
        let client = self.runtime.client.lock().clone();
        let Some(client) = client else {
            self.pause_capture();
            self.retire_decoders();
            return status;
        };
        match client.status() {
            Status::Live => {}
            Status::Connecting => {
                self.pause_capture();
                self.retire_decoders();
                return if status == Ok(CallState::Joining) {
                    status
                } else {
                    Ok(CallState::Reconnecting)
                };
            }
            _ => {
                self.pause_capture();
                self.retire_decoders();
                return Ok(CallState::Reconnecting);
            }
        }
        let generation = self.runtime.ticket.carrier_generation();
        if self.decoded_generation != generation {
            for publisher in self.publishers.drain(..) {
                self.audio.remove_publisher(&publisher);
            }
            self.decoded_generation = generation;
        }
        if let Some(intent) = &self.capture
            && (!intent.slot.current.load(Ordering::SeqCst)
                || !(intent.allowed)()
                || !intent.source.current())
        {
            self.stop_capture();
            return Err(Issue::PermissionDenied);
        }
        self.install_prepared_capture(&client)?;
        if let Some(intent) = &self.capture {
            let installed = intent.slot.generation.lock().is_some();
            if installed {
                for frame in self
                    .audio
                    .poll_capture(now)
                    .map_err(|_| Issue::CaptureFailed)?
                {
                    let result = client.try_send(client::CapturedOpus {
                        timestamp_48k: frame.timestamp_48k,
                        duration_samples: frame.duration_samples,
                        payload: frame.payload,
                        captured_at: frame.captured_at,
                    });
                    if !matches!(result, Ok(()) | Err(client::ClientError::Expired)) {
                        self.pause_capture();
                        return Ok(CallState::Reconnecting);
                    }
                }
                if self.audio.capture_ready() {
                    // The watchdog cannot race a new start: startup changes
                    // and readiness publication share the generation lock.
                    let _generation = intent.slot.generation.lock();
                    if intent.slot.startup.load(Ordering::SeqCst) == intent.startup {
                        intent.slot.ready.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        // Eight publishers times ten bounded frames: one tick never drains an
        // unbounded remote queue, and every original 200ms deadline is retained.
        for _ in 0..80 {
            let Some(frame) = client.try_recv() else {
                break;
            };
            let publisher = voice::PublicationId {
                namespace: frame.publisher.namespace,
                carrier_generation: frame.publisher.carrier_generation,
            };
            if !self.publishers.contains(&publisher) {
                self.publishers.push(publisher.clone());
            }
            self.audio
                .receive(
                    voice::ReceivedOpus {
                        publisher,
                        sequence: frame.object.sequence,
                        timestamp_48k: frame.object.timestamp,
                        duration_samples: frame.object.duration,
                        payload: frame.object.payload,
                        received_at: frame.received_at,
                        expires_at: frame.expires_at,
                    },
                    now,
                )
                .map_err(|_| Issue::TransportFailed)?;
        }
        self.audio.playout(now).map_err(|_| Issue::Unavailable)?;
        Ok(CallState::Live)
    }
    fn inventory(&self) -> Inventory {
        if self.audio.output_ready()
            && self
                .runtime
                .client
                .lock()
                .as_ref()
                .is_some_and(|client| client.status() == Status::Live)
        {
            authority::inventory(&self.runtime.ticket)
        } else {
            Inventory::default()
        }
    }
    fn start_capture(
        &mut self,
        source_id: &str,
        handle: BorrowedCaptureHandle<'_>,
        allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), Issue> {
        let BorrowedCaptureHandle::PipeWireNode(node) = handle else {
            return Err(Issue::SourceLost);
        };
        let source = self
            .devices
            .source(source_id, node)
            .ok_or(Issue::SourceLost)?;
        let client = self
            .runtime
            .client
            .lock()
            .clone()
            .ok_or(Issue::TransportFailed)?;
        if client.status() != Status::Live || !allowed() {
            return Err(Issue::PermissionDenied);
        }
        let reuse = self.capture.as_ref().is_some_and(|intent| {
            intent.source_id == source_id
                && Arc::ptr_eq(&intent.allowed, &allowed)
                && Arc::ptr_eq(&intent.source, &source)
                && intent.slot.current.load(Ordering::SeqCst)
        });
        if !reuse {
            // Leaves `self.capture` empty, so the intent below is always fresh.
            self.stop_capture();
        }
        let intent = self.capture.get_or_insert_with(|| CaptureIntent {
            source_id: source_id.into(),
            source,
            allowed: allowed.clone(),
            slot: Arc::new(StopSlot::new()),
            startup: 0,
            pending: None,
            prepared: None,
            watchdog: None,
        });
        if intent.pending.is_some()
            || intent.prepared.is_some()
            || intent.slot.generation.lock().is_some()
        {
            return Ok(());
        }
        intent.cancel_pending();
        let slot = intent.slot.clone();
        let startup = slot.begin()?;
        intent.startup = startup;
        let weak = Arc::downgrade(&slot);
        self.runtime
            .ticket
            .set_capture_stop(Arc::new(move |terminal| {
                if let Some(slot) = weak.upgrade() {
                    if terminal {
                        slot.stop();
                    } else {
                        slot.pause();
                    }
                }
            }));
        let devices = self.devices.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let preparing = self.runtime.executor.spawn(async move {
            let _ = send.send(devices.prepare().await);
        });
        intent.pending = Some(PendingCapture {
            receive,
            task: preparing.abort_handle(),
        });
        let started = tokio::time::Instant::now();
        let deadline = started
            .checked_add(Duration::from_secs(2))
            .unwrap_or(started);
        let watchdog = self.runtime.executor.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            slot.expire(startup);
        });
        intent.watchdog = Some(watchdog.abort_handle());
        Ok(())
    }
    fn capture_stop(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let slot = self.capture.as_ref()?.slot.clone();
        Some(Arc::new(move || slot.stop()))
    }
    fn capture_ready(&self) -> bool {
        self.audio.capture_ready()
            && self.capture.as_ref().is_some_and(|intent| {
                intent.slot.current.load(Ordering::SeqCst)
                    && intent.slot.ready.load(Ordering::SeqCst)
                    && intent.slot.startup.load(Ordering::SeqCst) == intent.startup
                    && intent.source.current()
                    && (intent.allowed)()
            })
    }
    fn pause_capture(&mut self) {
        if let Some(intent) = &mut self.capture {
            intent.cancel_pending();
            intent.slot.pause();
        }
        self.audio.stop_capture();
    }
    fn stop_capture(&mut self) {
        if let Some(intent) = self.capture.take() {
            intent.slot.stop();
        }
        self.audio.stop_capture();
    }
    fn close(&mut self) {
        self.stop_capture();
        let first = !self.runtime.cancelled.swap(true, Ordering::SeqCst);
        self.runtime.ticket.stop();
        let task = self.runtime.task.lock().take();
        if let Some(task) = task {
            task.abort();
        }
        let client = self.runtime.client.lock().take();
        if let Some(client) = client {
            client.close();
        }
        self.audio.close();
        *self.runtime.state.lock() = Ok(CallState::Ended);
        if first {
            let ticket = self.runtime.ticket.clone();
            let commands = self.runtime.commands.clone();
            // Best-effort leave; the local stop above never waits for the
            // network, and the governor ends a silent participation anyway.
            self.runtime.executor.spawn(async move {
                let _ = command(&commands, Operation::End { ticket }).await;
            });
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_startup_timeout_cannot_revoke_resumed_intent_but_current_timeout_does() {
        let slot = StopSlot::new();
        let old = slot.begin().unwrap();
        slot.pause();
        let resumed = slot.begin().unwrap();
        slot.expire(old);
        assert!(slot.current.load(Ordering::SeqCst));
        slot.expire(resumed);
        assert!(!slot.current.load(Ordering::SeqCst));
        assert!(
            slot.begin().is_err(),
            "deadline is terminal for this unmute"
        );
    }

    #[test]
    fn observed_capture_readiness_survives_its_deadline_but_leave_is_terminal() {
        let slot = StopSlot::new();
        let startup = slot.begin().unwrap();
        slot.ready.store(true, Ordering::SeqCst);
        slot.expire(startup);
        assert!(slot.current.load(Ordering::SeqCst));
        slot.stop();
        assert!(!slot.current.load(Ordering::SeqCst));
        assert!(slot.begin().is_err());
    }
}
