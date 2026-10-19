//! Exercise the service with the real lifecycle owner, prompt queue and lease broker.
//! The device backend is controlled; these tests do not qualify physical audio.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use super::super::collab_broker_capnp::LocalSessionAuthority;
use super::*;
use crate::portal::{LocalCaptureHandle, PromptOutcome, PromptResolver};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Device {
    starts: AtomicUsize,
    stops: AtomicUsize,
    closes: AtomicUsize,
    reconnecting: AtomicBool,
    pending: AtomicBool,
    ticks: AtomicUsize,
    /// Who the controlled call hears talking, and its revision.
    speaking: Mutex<Vec<String>>,
    speaking_revision: std::sync::atomic::AtomicU64,
    /// The sources the controlled call publishes, and how often each started.
    video: Mutex<Vec<PictureSource>>,
    video_starts: AtomicUsize,
    /// What the app shows, as the call was last told.
    shown: Mutex<Vec<Shown>>,
    /// Pictures waiting for the app.
    pictures: Mutex<Vec<Picture>>,
    arrived: Arc<tokio::sync::Notify>,
}
struct BackendFixture(Arc<Device>);
impl MediaSession for BackendFixture {
    fn capture_ready(&self) -> bool {
        self.0.starts.load(Ordering::SeqCst) > 0 && !self.0.pending.load(Ordering::SeqCst)
    }
    fn inventory(&self) -> Inventory {
        Inventory::default()
    }
    fn tick(&mut self, _: Instant) -> Result<State, Issue> {
        self.0.ticks.fetch_add(1, Ordering::SeqCst);
        Ok(if self.0.reconnecting.load(Ordering::SeqCst) {
            State::Reconnecting
        } else {
            State::Live
        })
    }
    #[allow(
        clippy::panic_in_result_fn,
        reason = "the test backend asserts the broker handed it exactly the leased synthetic source"
    )]
    fn start_capture(
        &mut self,
        source_id: &str,
        source: BorrowedCaptureHandle<'_>,
        current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), Issue> {
        assert_eq!(source_id, "mic");
        assert!(matches!(source, BorrowedCaptureHandle::PipeWireNode(17)));
        assert!(current());
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn capture_stop(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let device = self.0.clone();
        Some(Arc::new(move || {
            device.stops.fetch_add(1, Ordering::SeqCst);
        }))
    }
    fn stop_capture(&mut self) {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn pause_capture(&mut self) {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn close(&mut self) {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
    }
    fn speaking_revision(&self) -> u64 {
        self.0.speaking_revision.load(Ordering::SeqCst)
    }
    fn speaking(&self) -> Vec<String> {
        self.0.speaking.lock().unwrap().clone()
    }
    fn start_video(&mut self, source: PictureSource) -> Result<(), Issue> {
        self.0.video_starts.fetch_add(1, Ordering::SeqCst);
        self.0.video.lock().unwrap().push(source);
        Ok(())
    }
    fn stop_video(&mut self, source: PictureSource) {
        self.0.video.lock().unwrap().retain(|on| *on != source);
    }
    fn show(&mut self, shown: Vec<Shown>) {
        *self.0.shown.lock().unwrap() = shown;
    }
    fn take_pictures(&mut self, after: u64, _budget: usize) -> (Vec<Picture>, u64) {
        let pictures = std::mem::take(&mut *self.0.pictures.lock().unwrap());
        let cursor = after.saturating_add(u64::try_from(pictures.len()).unwrap());
        (pictures, cursor)
    }
    fn pictures_signal(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.0.arrived.clone())
    }
}
struct Factory(Arc<Device>);
impl MediaFactory for Factory {
    fn prepare(&self, target: CallTarget) -> Prepare {
        assert_eq!(target.app, "so.tilo.chat");
        assert_eq!(target.record, ""); // Primary record remains an exact binding.
        assert_eq!(target.channel, "voice");
        let backend = BackendFixture(Arc::clone(&self.0));
        Box::pin(async move { Ok(Box::new(backend) as Box<dyn MediaSession>) })
    }
}
struct Fixture {
    owner: LocalSessionAuthority,
    lifecycle: LocalSessionLifecycle,
    service: NativeMediaService,
    caller: Caller,
    prompts: PromptQueueHandle,
    device: Arc<Device>,
    next: std::sync::atomic::AtomicU64,
}
impl Fixture {
    fn tick(&self) {
        let generation = {
            let mut calls = self.service.0.calls.lock().unwrap();
            let Some(entry) = calls.entries.get_mut(&self.caller.key()) else {
                return;
            };
            if let Some(worker) = entry.worker.take() {
                worker.abort();
            }
            entry.generation
        };
        self.service.0.tick(&self.caller.key(), generation);
    }
    fn new() -> Self {
        let owner = LocalSessionAuthority::new();
        let lifecycle = owner.lifecycle();
        lifecycle.publish_native_state(false, false);
        let workspace = super::super::state::WorkspaceTree::bootstrap_demo()
            .desktop
            .workspaces
            .remove(0);
        let room = workspace.room.to_string();
        let (grants, _) = GrantStoreHandle::test_with_workspace(workspace);
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let mut capture = CaptureBroker::new();
        capture
            .register_source(
                "mic",
                "Microphone",
                CaptureKind::Microphone,
                LocalCaptureHandle::PipeWireNode(17),
            )
            .unwrap();
        let service = NativeMediaService::new(
            lifecycle.clone(),
            Arc::new(Mutex::new(capture)),
            super::super::personal_continuity::DeviceId::new("device-test").unwrap(),
        );
        assert!(service.install_consent(grants, prompts.clone()));
        let device = Arc::new(Device::default());
        assert!(service.install_factory(Arc::new(Factory(Arc::clone(&device)))));
        // These cases drive the microphone by hand: the person here chose
        // to join muted. Joining with it on has its own cases below.
        service.preferences().remember(true);
        let caller = Caller {
            peer: 11,
            app: "so.tilo.chat".into(),
            workspace: room,
            record: String::new(),
            current: Arc::new(|| Ok(())),
        };
        Self {
            owner,
            lifecycle,
            service,
            caller,
            prompts,
            device,
            next: std::sync::atomic::AtomicU64::new(0),
        }
    }
    fn command(&self, command: Command) -> Snapshot {
        self.service
            .command_for(self.caller.clone(), self.request(command))
            .unwrap()
    }
    fn request(&self, command: Command) -> Request {
        let mut request = Request::new(command);
        if request.carries_intent() {
            request.intent = self.next.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        }
        request
    }
    async fn join(&self) {
        let joined = self.command(Command::Join {
            channel_id: "voice".into(),
        });
        assert!(joined.muted);
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if self.command(Command::Status).state == State::Live {
                return;
            }
        }
        panic!("controlled backend did not become ready");
    }
    async fn approve_unmute(&self) {
        let before = self.device.starts.load(Ordering::SeqCst);
        self.command(Command::SetMuted { muted: false });
        let prompt = self
            .prompts
            .drain()
            .pop()
            .expect("real local consent queued");
        prompt.reply.send(PromptOutcome::AllowOnce).unwrap();
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if self.device.starts.load(Ordering::SeqCst) > before {
                return;
            }
        }
        panic!("approved capture request was not accepted");
    }
    async fn unmute(&self) {
        self.approve_unmute().await;
        assert!(self.command(Command::Status).capture_active);
    }
}

#[tokio::test]
async fn accepted_pending_capture_is_inactive_until_backend_observes_readiness() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.device.pending.store(true, Ordering::SeqCst);
    fixture.approve_unmute().await;
    for _ in 0..3 {
        fixture.tick();
        let pending = fixture.command(Command::Status);
        assert_eq!(pending.state, State::Live);
        assert!(!pending.muted);
        assert!(!pending.capture_active);
        assert_eq!(
            fixture.device.starts.load(Ordering::SeqCst),
            1,
            "pending is not paused recovery and must not request another start"
        );
    }
    fixture.device.pending.store(false, Ordering::SeqCst);
    fixture.tick();
    assert!(fixture.command(Command::Status).capture_active);
    fixture.command(Command::Leave);
    assert!(!fixture.command(Command::Status).capture_active);
}

#[tokio::test]
async fn every_pending_tick_requires_the_exact_existing_capture_binding() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.device.pending.store(true, Ordering::SeqCst);
    fixture.approve_unmute().await;
    fixture.tick();
    let other = Fixture::new();
    other.join().await;
    let other_binding = other
        .service
        .0
        .calls
        .lock()
        .unwrap()
        .entries
        .get(&other.caller.key())
        .unwrap()
        .binding
        .clone();
    {
        let mut calls = fixture.service.0.calls.lock().unwrap();
        let entry = calls.entries.get_mut(&fixture.caller.key()).unwrap();
        let capture = entry.capture.as_ref().unwrap();
        assert!((capture.fence)());
        let broker = fixture.service.0.capture.lock().unwrap();
        assert!(
            broker
                .resolve(capture.lease, &entry.binding, Instant::now())
                .is_ok()
        );
        assert!(
            broker
                .resolve(capture.lease, &other_binding, Instant::now())
                .is_err()
        );
        // Fault-inject an actual second local session's binding. The lease is
        // still live, but must never be used by this queued backend operation.
        entry.binding = other_binding;
    }
    let before = fixture.device.ticks.load(Ordering::SeqCst);
    fixture.device.pending.store(false, Ordering::SeqCst);
    fixture.tick();
    assert_eq!(fixture.device.ticks.load(Ordering::SeqCst), before);
    assert!(!fixture.command(Command::Status).capture_active);
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn unmute_during_reconnect_keeps_the_call_without_consent_or_deferred_capture() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.device.reconnecting.store(true, Ordering::SeqCst);
    fixture.tick();
    let pending = fixture.command(Command::SetMuted { muted: false });
    assert_eq!(pending.state, State::Reconnecting);
    assert!(pending.muted);
    assert!(!pending.capture_active);
    assert_eq!(fixture.prompts.queued_len(), 0);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.device.closes.load(Ordering::SeqCst), 0);
    fixture.device.reconnecting.store(false, Ordering::SeqCst);
    fixture.tick();
    let recovered = fixture.command(Command::Status);
    assert_eq!(recovered.state, State::Live);
    assert!(recovered.muted);
    assert!(!recovered.capture_active);
    assert_eq!(fixture.prompts.queued_len(), 0);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
    fixture.unmute().await;
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 1);
    fixture.lifecycle.set_locked(true);
    assert!(
        fixture
            .service
            .command_for(
                fixture.caller.clone(),
                fixture.request(Command::SetMuted { muted: false })
            )
            .is_err()
    );
    assert_eq!(fixture.prompts.queued_len(), 0);
    assert!(!fixture.command(Command::Status).capture_active);
}

#[tokio::test]
async fn transport_recovery_pauses_then_resolves_the_same_lease_without_new_consent() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    let lease = fixture
        .service
        .0
        .calls
        .lock()
        .unwrap()
        .entries
        .get(&fixture.caller.key())
        .unwrap()
        .capture
        .as_ref()
        .unwrap()
        .lease;
    fixture.device.reconnecting.store(true, Ordering::SeqCst);
    fixture.tick();
    let paused = fixture.command(Command::Status);
    assert_eq!(paused.state, State::Reconnecting);
    assert!(!paused.capture_active);
    assert!(
        !paused.muted,
        "same explicit unmute intent survives transport pause"
    );
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
    assert_eq!(fixture.prompts.queued_len(), 0);
    fixture.device.reconnecting.store(false, Ordering::SeqCst);
    fixture.tick();
    assert!(fixture.command(Command::Status).capture_active);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.prompts.queued_len(), 0);
    assert_eq!(
        fixture
            .service
            .0
            .calls
            .lock()
            .unwrap()
            .entries
            .get(&fixture.caller.key())
            .unwrap()
            .capture
            .as_ref()
            .unwrap()
            .lease,
        lease
    );
}

#[tokio::test]
async fn terminal_local_invalidation_wins_over_transport_recovery() {
    for invalidation in 0..5 {
        let fixture = Fixture::new();
        fixture.join().await;
        fixture.unmute().await;
        fixture.device.reconnecting.store(true, Ordering::SeqCst);
        fixture.tick();
        match invalidation {
            0 => {
                fixture.command(Command::Leave);
            }
            1 => {
                fixture.command(Command::SetMuted { muted: true });
            }
            2 => fixture
                .service
                .0
                .apply_privacy(&crate::portal::lockdown::LockdownPolicy {
                    disable_microphone: true,
                    ..Default::default()
                }),
            3 => {
                fixture
                    .service
                    .0
                    .capture
                    .lock()
                    .unwrap()
                    .remove_source("mic");
            }
            _ => fixture.lifecycle.set_locked(true),
        }
        fixture.device.reconnecting.store(false, Ordering::SeqCst);
        fixture.tick();
        assert!(!fixture.command(Command::Status).capture_active);
        assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.prompts.queued_len(), 0);
    }
}

#[tokio::test]
async fn join_is_listening_and_muted_until_fresh_local_consent() {
    let fixture = Fixture::new();
    fixture.join().await;
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.prompts.queued_len(), 0);
    fixture.unmute().await;
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 1);
    fixture.command(Command::SetMuted { muted: true });
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
    fixture.command(Command::SetMuted { muted: false });
    assert_eq!(fixture.prompts.queued_len(), 1);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn declined_or_late_consent_cannot_create_capture() {
    for leave in [false, true] {
        let fixture = Fixture::new();
        fixture.join().await;
        fixture.command(Command::SetMuted { muted: false });
        let prompt = fixture.prompts.drain().pop().unwrap();
        if leave {
            fixture.command(Command::Leave);
        }
        let _ = prompt.reply.send(if leave {
            PromptOutcome::AllowOnce
        } else {
            PromptOutcome::Deny
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
        assert!(!fixture.command(Command::Status).capture_active);
    }
}

#[tokio::test]
async fn actual_lock_transition_stops_device_and_revokes_lease_before_return() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    let (lease, binding) = {
        let calls = fixture.service.0.calls.lock().unwrap();
        let entry = calls.entries.get(&fixture.caller.key()).unwrap();
        (entry.capture.as_ref().unwrap().lease, entry.binding.clone())
    };
    fixture.lifecycle.set_locked(true);
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
    assert!(fixture.device.closes.load(Ordering::SeqCst) > 0);
    assert!(
        fixture
            .service
            .0
            .capture
            .lock()
            .unwrap()
            .resolve(lease, &binding, Instant::now())
            .is_err()
    );
    fixture.lifecycle.set_locked(false);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.command(Command::Status).state, State::Idle);
}

#[tokio::test]
async fn broker_source_loss_and_privacy_switch_stop_capture_without_another_rpc() {
    for privacy in [false, true] {
        let fixture = Fixture::new();
        fixture.join().await;
        fixture.unmute().await;
        let before = fixture.device.stops.load(Ordering::SeqCst);
        let mut broker = fixture.service.0.capture.lock().unwrap();
        if privacy {
            broker.set_kill_switch(CaptureKind::Microphone, true);
        } else {
            assert!(broker.remove_source("mic"));
        }
        assert!(fixture.device.stops.load(Ordering::SeqCst) > before);
    }
}

#[tokio::test]
async fn disconnect_and_owner_drop_stop_capture_synchronously() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    fixture.service.disconnect(fixture.caller.peer);
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
    assert!(fixture.device.closes.load(Ordering::SeqCst) > 0);
    assert!(fixture.service.0.calls.lock().unwrap().entries.is_empty());
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    drop(fixture.owner);
    assert!(fixture.device.stops.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn absent_real_adapter_is_unavailable_and_never_prompts() {
    let fixture = Fixture::new();
    let service = NativeMediaService::new(
        fixture.lifecycle.clone(),
        fixture.service.capture_broker(),
        super::super::personal_continuity::DeviceId::new("device-test").unwrap(),
    );
    let snapshot = service
        .command_for(
            fixture.caller.clone(),
            fixture.request(Command::Join {
                channel_id: "voice".into(),
            }),
        )
        .unwrap();
    assert_eq!(snapshot.state, State::Unavailable);
    assert_eq!(fixture.prompts.queued_len(), 0);
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reordered_effects_cannot_cross_a_later_leave_or_mute() {
    for leave in [false, true] {
        let fixture = Fixture::new();
        fixture.join().await;
        let old_unmute = fixture.request(Command::SetMuted { muted: false });
        let old_join = fixture.request(Command::Join {
            channel_id: "voice".into(),
        });
        fixture.command(if leave {
            Command::Leave
        } else {
            Command::SetMuted { muted: true }
        });
        for stale in [old_unmute, old_join] {
            assert!(
                fixture
                    .service
                    .command_for(fixture.caller.clone(), stale)
                    .unwrap_err()
                    .contains("stale")
            );
        }
        assert_eq!(fixture.prompts.queued_len(), 0);
        assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
        assert!(!fixture.command(Command::Status).capture_active);
    }
}

#[tokio::test]
async fn pending_consent_is_fenced_by_newer_mute_and_live_admission_loss() {
    for mute in [false, true] {
        let mut fixture = Fixture::new();
        let admitted = Arc::new(AtomicBool::new(true));
        let checked = admitted.clone();
        fixture.caller.current = Arc::new(move || {
            checked
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "admission ended".into())
        });
        fixture.join().await;
        fixture.command(Command::SetMuted { muted: false });
        let prompt = fixture.prompts.drain().pop().unwrap();
        if mute {
            fixture.command(Command::SetMuted { muted: true });
        } else {
            admitted.store(false, Ordering::Release);
        }
        let _ = prompt.reply.send(PromptOutcome::AllowOnce);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn source_revocation_does_not_acquire_the_outer_backend_mutex() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    let backend = fixture
        .service
        .0
        .calls
        .lock()
        .unwrap()
        .entries
        .get(&fixture.caller.key())
        .unwrap()
        .backend
        .clone()
        .unwrap();
    let _held = backend.lock().unwrap();
    let before = fixture.device.stops.load(Ordering::SeqCst);
    let capture = fixture.service.capture_broker();
    let (stopped, observed) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        assert!(capture.lock().unwrap().remove_source("mic"));
        stopped.send(()).unwrap();
    });
    tilo_test_env::recv(
        "lease revocation must not wait on the outer backend lock",
        &observed,
    );
    assert!(fixture.device.stops.load(Ordering::SeqCst) > before);
    thread.join().unwrap();
}

#[tokio::test]
async fn committed_local_privacy_policy_stops_capture_before_setter_returns() {
    let fixture = Fixture::new();
    fixture.join().await;
    fixture.unmute().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lockdown.json");
    let shared = Arc::downgrade(&fixture.service.0);
    let listener: Arc<dyn Fn(&crate::portal::lockdown::LockdownPolicy) + Send + Sync> =
        Arc::new(move |policy| {
            if let Some(shared) = shared.upgrade() {
                shared.apply_privacy(policy);
            }
        });
    crate::portal::lockdown::watch_policy_at(path.clone(), &listener).unwrap();
    let before = fixture.device.stops.load(Ordering::SeqCst);
    let backend = crate::portal::lockdown::LockdownBackend::at_path(path);
    backend
        .set_policy(crate::portal::lockdown::LockdownPolicy {
            disable_microphone: true,
            ..Default::default()
        })
        .unwrap();
    assert!(fixture.device.stops.load(Ordering::SeqCst) > before);
    assert!(!fixture.command(Command::Status).capture_active);
    backend
        .set_policy(crate::portal::lockdown::LockdownPolicy::default())
        .unwrap();
    assert_eq!(
        fixture.device.starts.load(Ordering::SeqCst),
        1,
        "restoring policy cannot implicitly unmute"
    );
    fixture.command(Command::SetMuted { muted: false });
    let prompt = fixture.prompts.drain().pop().unwrap();
    backend
        .set_policy(crate::portal::lockdown::LockdownPolicy {
            disable_microphone: true,
            ..Default::default()
        })
        .unwrap();
    backend
        .set_policy(crate::portal::lockdown::LockdownPolicy::default())
        .unwrap();
    let _ = prompt.reply.send(PromptOutcome::AllowOnce);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        fixture.device.starts.load(Ordering::SeqCst),
        1,
        "privacy revocation must fence pending consent even after policy is restored"
    );
}

struct GatedFactory {
    device: Arc<Device>,
    ready: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl MediaFactory for GatedFactory {
    fn prepare(&self, _: CallTarget) -> Prepare {
        let ready = self.ready.lock().unwrap().take().unwrap();
        let device = self.device.clone();
        Box::pin(async move {
            ready.await.map_err(|_| Issue::TransportFailed)?;
            Ok(Box::new(BackendFixture(device)) as Box<dyn MediaSession>)
        })
    }
}

#[tokio::test]
async fn pending_join_cannot_install_a_backend_after_leave() {
    let fixture = Fixture::new();
    let service = NativeMediaService::new(
        fixture.lifecycle.clone(),
        fixture.service.capture_broker(),
        super::super::personal_continuity::DeviceId::new("device-test").unwrap(),
    );
    let consent = fixture.service.0.consent.get().unwrap().clone();
    assert!(service.install_consent(consent.grants, consent.prompts));
    let (ready, waiting) = tokio::sync::oneshot::channel();
    assert!(service.install_factory(Arc::new(GatedFactory {
        device: fixture.device.clone(),
        ready: Mutex::new(Some(waiting))
    })));
    let joining = service
        .command_for(
            fixture.caller.clone(),
            fixture.request(Command::Join {
                channel_id: "voice".into(),
            }),
        )
        .unwrap();
    assert_eq!(joining.state, State::Joining);
    tokio::task::yield_now().await;
    service
        .command_for(fixture.caller.clone(), fixture.request(Command::Leave))
        .unwrap();
    let _ = ready.send(());
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(service.0.calls.lock().unwrap().entries.is_empty());
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.prompts.queued_len(), 0);
}

async fn settle(until: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if until() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    until()
}

#[tokio::test]
async fn joining_turns_the_microphone_on_asking_once_and_a_mute_is_remembered() {
    let fixture = Fixture::new();
    fixture.service.preferences().remember(false);
    // Joining is being in the call with the microphone on: once the call is
    // live, the normal permission prompt asks for it.
    fixture.join().await;
    assert!(
        settle(|| fixture.prompts.queued_len() == 1).await,
        "the join asks for the microphone"
    );
    let prompt = fixture.prompts.drain().pop().unwrap();
    prompt.reply.send(PromptOutcome::AllowAlways).unwrap();
    assert!(settle(|| fixture.device.starts.load(Ordering::SeqCst) == 1).await);
    assert!(!fixture.command(Command::Status).muted);
    // The allowance covers the next join: no second prompt.
    fixture.command(Command::Leave);
    fixture.join().await;
    assert!(settle(|| fixture.device.starts.load(Ordering::SeqCst) == 2).await);
    assert_eq!(fixture.prompts.queued_len(), 0);
    // Muting is the person's choice, kept for the calls after this one.
    fixture.command(Command::SetMuted { muted: true });
    assert!(fixture.service.preferences().join_muted());
    fixture.command(Command::Leave);
    fixture.join().await;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(fixture.device.starts.load(Ordering::SeqCst), 2);
    assert!(fixture.command(Command::Status).muted);
}

#[tokio::test]
async fn a_microphone_allowed_once_is_asked_for_again_on_the_next_join() {
    let fixture = Fixture::new();
    fixture.service.preferences().remember(false);
    fixture.join().await;
    assert!(settle(|| fixture.prompts.queued_len() == 1).await);
    let prompt = fixture.prompts.drain().pop().unwrap();
    prompt.reply.send(PromptOutcome::AllowOnce).unwrap();
    assert!(settle(|| fixture.device.starts.load(Ordering::SeqCst) == 1).await);
    fixture.command(Command::Leave);
    fixture.join().await;
    assert!(settle(|| fixture.prompts.queued_len() == 1).await);
}

#[tokio::test]
async fn a_watch_answers_when_the_call_hears_someone_and_holds_while_nothing_changes() {
    let fixture = Fixture::new();
    fixture.join().await;
    let route = (
        fixture.caller.workspace.clone(),
        fixture.caller.app.clone(),
        fixture.caller.record.clone(),
    );
    let seen = fixture
        .service
        .watch(fixture.caller.clone(), 0, route.clone())
        .await
        .unwrap();
    assert_ne!(
        seen.revision, 0,
        "a watcher with nothing is answered at once"
    );
    assert_eq!(seen.state, State::Live);
    assert!(seen.speaking.is_empty());

    let held = fixture
        .service
        .watch(fixture.caller.clone(), seen.revision, route);
    tokio::pin!(held);
    fixture.tick();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut held)
            .await
            .is_err(),
        "nothing changed: the watch is held"
    );

    // Ana talks: the next tick sees it and the held watch answers.
    let ana = "a".repeat(64);
    *fixture.device.speaking.lock().unwrap() = vec![ana.clone()];
    fixture
        .device
        .speaking_revision
        .fetch_add(1, Ordering::SeqCst);
    fixture.tick();
    let answered = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("the watch answers")
        .unwrap();
    assert_eq!(answered.speaking, vec![ana]);
    assert_ne!(answered.revision, seen.revision);
}

impl Fixture {
    /// A camera and a screen the broker knows, as the system's defaults.
    fn with_camera_and_screen(self) -> Self {
        let broker = self.service.capture_broker();
        let mut broker = broker.lock().unwrap();
        for (id, kind) in [
            ("default-camera", CaptureKind::Camera),
            ("primary-display", CaptureKind::Screen),
        ] {
            broker
                .register_source(id, "Default", kind, LocalCaptureHandle::SystemDefault(kind))
                .unwrap();
        }
        drop(broker);
        self
    }
    async fn video_started(&self, count: usize) {
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if self.device.video_starts.load(Ordering::SeqCst) >= count {
                return;
            }
        }
        panic!("the approved picture did not start");
    }
}

#[tokio::test]
async fn the_camera_turns_on_through_the_normal_prompt_shows_as_capturing_and_turns_off_at_once() {
    let fixture = Fixture::new().with_camera_and_screen();
    fixture.join().await;
    let asked = fixture.command(Command::SetCamera { on: true });
    assert!(!asked.camera, "nothing shows before the person answers");
    let prompt = fixture
        .prompts
        .drain()
        .pop()
        .expect("the camera is asked for");
    assert!(matches!(
        prompt.capability,
        super::super::state::Capability::Camera { .. }
    ));
    prompt.reply.send(PromptOutcome::AllowOnce).unwrap();
    fixture.video_started(1).await;
    let on = fixture.command(Command::Status);
    assert!(on.camera && !on.screen);
    // While it is on, the camera is captured under a lease: what the system
    // shows as capturing.
    let events = fixture
        .service
        .capture_broker()
        .lock()
        .unwrap()
        .take_privacy_events();
    assert!(events.iter().any(|event| matches!(
        event,
        crate::portal::capture_broker::CapturePrivacyEvent::Started {
            kind: CaptureKind::Camera,
            ..
        }
    )));
    let off = fixture.command(Command::SetCamera { on: false });
    assert!(!off.camera);
    assert!(fixture.device.video.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_camera_refused_at_the_prompt_stays_off_and_says_so() {
    let fixture = Fixture::new().with_camera_and_screen();
    fixture.join().await;
    fixture.command(Command::SetScreen { on: true });
    let prompt = fixture
        .prompts
        .drain()
        .pop()
        .expect("screen sharing is asked for");
    assert!(matches!(
        prompt.capability,
        super::super::state::Capability::Screencast { .. }
    ));
    prompt.reply.send(PromptOutcome::Deny).unwrap();
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let status = fixture.command(Command::Status);
    assert!(!status.screen);
    assert_eq!(status.issue, Some(Issue::ScreenDenied));
    assert_eq!(fixture.device.video_starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn turning_the_camera_on_never_cancels_the_microphones_prompt_or_the_join() {
    let fixture = Fixture::new().with_camera_and_screen();
    fixture.join().await;
    fixture.command(Command::SetMuted { muted: false });
    fixture.command(Command::SetCamera { on: true });
    let prompts = fixture.prompts.drain();
    assert_eq!(prompts.len(), 2, "both are asked for, side by side");
    for prompt in prompts {
        prompt.reply.send(PromptOutcome::AllowOnce).unwrap();
    }
    fixture.video_started(1).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if fixture.device.starts.load(Ordering::SeqCst) > 0 {
            break;
        }
    }
    assert_eq!(
        fixture.device.starts.load(Ordering::SeqCst),
        1,
        "the microphone started"
    );
    assert!(fixture.command(Command::Status).camera);
}

#[tokio::test]
async fn a_camera_switched_off_by_policy_stops_with_the_next_tick() {
    let fixture = Fixture::new().with_camera_and_screen();
    fixture.join().await;
    fixture.command(Command::SetCamera { on: true });
    fixture
        .prompts
        .drain()
        .pop()
        .unwrap()
        .reply
        .send(PromptOutcome::AllowOnce)
        .unwrap();
    fixture.video_started(1).await;
    fixture
        .service
        .0
        .apply_privacy(&crate::portal::lockdown::LockdownPolicy {
            disable_camera: true,
            ..Default::default()
        });
    fixture.tick();
    let status = fixture.command(Command::Status);
    assert!(!status.camera);
    assert_eq!(status.issue, Some(Issue::CameraLost));
    assert!(fixture.device.video.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_app_says_what_it_shows_and_collects_pictures_as_they_arrive() {
    let fixture = Fixture::new();
    fixture.join().await;
    let shown = vec![Shown {
        person: "ab".repeat(32),
        source: PictureSource::Camera,
        layer: runtime_collab::native_media::PictureLayer::Mid,
    }];
    let showing = fixture
        .service
        .command_for(
            fixture.caller.clone(),
            fixture.request(Command::Show {
                pictures: shown.clone(),
            }),
        )
        .unwrap();
    assert_eq!(showing.state, State::Live);
    assert_eq!(*fixture.device.shown.lock().unwrap(), shown);

    // A collection waits for a picture, and is answered the moment one
    // arrives.
    let waiting = fixture.service.pictures(fixture.caller.clone(), 0);
    let device = fixture.device.clone();
    let arrive = async move {
        tokio::task::yield_now().await;
        device.pictures.lock().unwrap().push(Picture {
            person: "ab".repeat(32),
            stream: "s".into(),
            source: PictureSource::Camera,
            object: vec![1; 40],
        });
        device.arrived.notify_waiters();
    };
    let (answer, ()) = tokio::join!(waiting, arrive);
    let answer = answer.unwrap();
    assert_eq!(answer.pictures.len(), 1);
    assert_eq!(answer.pictures_cursor, 1);
    assert!(answer.valid());
}

/// The channel this device is in, from the moment it joins until it
/// leaves: what this person's presence says as their call.
#[tokio::test]
async fn the_call_this_device_is_in_lasts_from_joining_until_leaving() {
    let fixture = Fixture::new();
    let room = fixture.caller.workspace.clone();
    assert_eq!(fixture.service.own_call(&room), None, "in no call yet");
    fixture.join().await;
    assert_eq!(
        fixture.service.own_call(&room),
        Some(("so.tilo.chat".into(), String::new(), "voice".into())),
        "joined: in that channel"
    );
    assert_eq!(fixture.service.own_call("another-room"), None);

    fixture.command(Command::Leave);
    assert_eq!(fixture.service.own_call(&room), None, "left: in no call");
}
