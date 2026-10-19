//! Target-local call lifecycle. Wire intent never grants microphone authority.
//!
//! Joining a voice channel is being in its call with the microphone on, as in
//! any voice chat, unless the person last chose to be muted: that choice is
//! theirs and remembered ([`CallPreferences`]). The microphone itself is
//! asked for through the normal permission prompt, once for as long as the
//! person allows it there, and while it captures the system shows it.
use super::collab_broker_capnp::{LocalSessionLifecycle, LocalSessionPermit};
use super::realtime_session::VerifiedAppRegistration;
use crate::portal::capture_broker::BorrowedCaptureHandle;
use crate::portal::{
    CaptureBroker, CaptureKind, CaptureLeaseId, CaptureSessionBinding, GrantStoreHandle,
    PortalCaller, PromptQueueHandle,
};
use runtime_collab::native_media::{
    Command, Input, Inventory, Issue, Request, Roster, Snapshot, State, WATCH_HOLD,
};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tilo_action_broker::services::{
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
};
use tilo_lock::MutexRecover as _;

const MAX_CALLS: usize = 16;
const CONSENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Inert exact lookup input. A factory must independently consult its real
/// current native owner; these strings are not media authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallTarget {
    pub workspace: String,
    pub app: String,
    pub record: String,
    pub channel: String,
}

pub(crate) type Prepare = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Box<dyn MediaSession>, Issue>> + Send>,
>;
/// Who is in the call of each voice channel of a target's record.
pub(crate) type Rosters = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Roster>> + Send>>;
pub(crate) trait MediaFactory: Send + Sync {
    fn prepare(&self, target: CallTarget) -> Prepare;
    /// The calls of `target`'s record (its `channel` is unused) as last
    /// heard from the room, sorted by channel. A factory without a room
    /// knows none.
    fn rosters(&self, _target: CallTarget) -> Rosters {
        Box::pin(async { Vec::new() })
    }
    /// Woken whenever a room's call list may have changed, so a voice
    /// channel shows a join or leave at once. A factory without a room has
    /// none.
    fn rosters_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

/// The person's own choice of joining calls muted: what they last chose
/// with the microphone button, kept across calls and restarts. Kept in
/// memory only until a file is installed (tests never touch a real one).
#[derive(Default)]
pub(crate) struct CallPreferences {
    muted: AtomicBool,
    path: OnceLock<std::path::PathBuf>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct CallPreferencesFile {
    join_muted: bool,
}
impl CallPreferences {
    /// Where the choice lives for this person: their private state.
    pub(crate) fn default_path() -> Option<std::path::PathBuf> {
        crate::portal::private_state_root().map(|root| root.join("tilo/calls.json"))
    }
    fn install(&self, path: std::path::PathBuf) -> bool {
        let muted = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CallPreferencesFile>(&bytes).ok())
            .is_some_and(|file| file.join_muted);
        self.muted.store(muted, Ordering::Release);
        self.path.set(path).is_ok()
    }
    pub(crate) fn join_muted(&self) -> bool {
        self.muted.load(Ordering::Acquire)
    }
    pub(crate) fn remember(&self, muted: bool) {
        if self.muted.swap(muted, Ordering::AcqRel) == muted {
            return;
        }
        if let Some(path) = self.path.get()
            && let Err(error) = tilo_files::native::atomic::json::write_private(
                path,
                &CallPreferencesFile { join_muted: muted },
            )
        {
            tracing::warn!(%error, "the call mute choice was not saved");
        }
    }
}
/// The installed owner/client/codec adapter. Starting a session always listens
/// muted. Stop methods synchronously fence capture/publication and cancel the
/// exact device connection; device-worker state changes are never awaited here.
pub(crate) trait MediaSession: Send {
    /// Bounded polling only: no external I/O, device state wait, or worker join.
    /// A retained capture lease and local permit guard every call.
    fn tick(&mut self, now: Instant) -> Result<State, Issue>;
    /// True only after the current device generation produced a fresh sample.
    fn capture_ready(&self) -> bool;
    fn inventory(&self) -> Inventory;
    fn start_capture(
        &mut self,
        source_id: &str,
        source: BorrowedCaptureHandle<'_>,
        current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), Issue>;
    /// Stops this exact capture generation without locking the outer backend.
    fn capture_stop(&self) -> Option<Arc<dyn Fn() + Send + Sync>>;
    /// Stop device delivery for transport recovery. A later start with the same
    /// source and fence must remain covered by the existing `capture_stop` handle.
    fn pause_capture(&mut self);
    fn stop_capture(&mut self);
    fn close(&mut self);
    /// Advances whenever who the call hears talking changes. Cheap: read on
    /// every tick.
    fn speaking_revision(&self) -> u64 {
        0
    }
    /// The people the call hears talking now (person ids, sorted, distinct),
    /// this device's person among them while its microphone is heard.
    fn speaking(&self) -> Vec<String> {
        Vec::new()
    }
}

type Backend = Arc<Mutex<Box<dyn MediaSession>>>;
type Revalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
#[derive(Clone)]
struct Caller {
    peer: u64,
    app: String,
    workspace: String,
    record: String,
    current: Revalidate,
}
impl From<NativeMediaRequestContext> for Caller {
    fn from(value: NativeMediaRequestContext) -> Self {
        Self {
            peer: value.peer_id(),
            app: value.app_id().into(),
            workspace: value.workspace_id().into(),
            record: value.record_id().into(),
            current: Arc::new(move || value.revalidate()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    peer: u64,
    app: String,
    workspace: String,
    record: String,
}
impl Caller {
    fn key(&self) -> Key {
        Key {
            peer: self.peer,
            app: self.app.clone(),
            workspace: self.workspace.clone(),
            record: self.record.clone(),
        }
    }
}
struct Capture {
    lease: CaptureLeaseId,
    source: String,
    current: Arc<AtomicBool>,
    fence: Arc<dyn Fn() -> bool + Send + Sync>,
    paused: bool,
}
struct Pending {
    key: String,
    task: tokio::task::AbortHandle,
}
struct Entry {
    intent: u64,
    generation: u64,
    caller: Caller,
    permit: LocalSessionPermit,
    binding: CaptureSessionBinding,
    snapshot: Snapshot,
    backend: Option<Backend>,
    capture: Option<Capture>,
    pending: Option<Pending>,
    worker: Option<tokio::task::AbortHandle>,
    /// The join turns the microphone on once the call is live.
    microphone_on_join: bool,
    /// The call went live with the microphone wanted: ask for it.
    unmute_due: bool,
    /// What the last tick saw of the call, to wake watchers when it changes.
    seen: Option<Seen>,
}
/// What a watcher of a call sees change between ticks.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    state: State,
    muted: bool,
    issue: Option<Issue>,
    capturing: bool,
    asking: bool,
    speaking: u64,
}
#[derive(Default)]
struct Calls {
    next: u64,
    entries: BTreeMap<Key, Entry>,
    floors: BTreeMap<Key, u64>,
}
#[derive(Clone)]
struct Consent {
    grants: GrantStoreHandle,
    prompts: PromptQueueHandle,
}
type PrivacyListener = Arc<dyn Fn(&crate::portal::lockdown::LockdownPolicy) + Send + Sync>;
struct Shared {
    lifecycle: LocalSessionLifecycle,
    target: super::personal_continuity::DeviceId,
    capture: Arc<Mutex<CaptureBroker>>,
    consent: OnceLock<Consent>,
    factory: OnceLock<Arc<dyn MediaFactory>>,
    calls: Mutex<Calls>,
    listener: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    privacy_listener: OnceLock<PrivacyListener>,
    output_disabled: AtomicBool,
    preferences: CallPreferences,
    /// The rosters of each caller's record, as last heard.
    rosters: Mutex<BTreeMap<(String, String, String), Vec<Roster>>>,
    /// Woken whenever a status may have changed: what a `Watch` waits on.
    changed: tokio::sync::Notify,
}
#[derive(Clone)]
pub(crate) struct NativeMediaService(Arc<Shared>);
impl NativeMediaService {
    pub(crate) fn new(
        lifecycle: LocalSessionLifecycle,
        capture: Arc<Mutex<CaptureBroker>>,
        target: super::personal_continuity::DeviceId,
    ) -> Self {
        let shared = Arc::new(Shared {
            lifecycle,
            target,
            capture,
            consent: OnceLock::new(),
            factory: OnceLock::new(),
            calls: Mutex::new(Calls::default()),
            listener: OnceLock::new(),
            privacy_listener: OnceLock::new(),
            output_disabled: AtomicBool::new(false),
            preferences: CallPreferences::default(),
            rosters: Mutex::new(BTreeMap::new()),
            changed: tokio::sync::Notify::new(),
        });
        let weak = Arc::downgrade(&shared);
        let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.stop_all();
            }
        });
        shared.lifecycle.watch(&listener);
        let _ = shared.listener.set(listener);
        let weak = Arc::downgrade(&shared);
        let privacy: Arc<dyn Fn(&crate::portal::lockdown::LockdownPolicy) + Send + Sync> =
            Arc::new(move |policy| {
                if let Some(shared) = weak.upgrade() {
                    shared.apply_privacy(policy);
                }
            });
        if crate::portal::lockdown::watch_policy(&privacy).is_err() {
            shared.apply_privacy(&crate::portal::lockdown::LockdownPolicy {
                disable_microphone: true,
                disable_sound_output: true,
                ..Default::default()
            });
        }
        let _ = shared.privacy_listener.set(privacy);
        Self(shared)
    }
    pub(crate) fn install_consent(
        &self,
        grants: GrantStoreHandle,
        prompts: PromptQueueHandle,
    ) -> bool {
        self.0.consent.set(Consent { grants, prompts }).is_ok()
    }
    pub(crate) fn install_factory(&self, factory: Arc<dyn MediaFactory>) -> bool {
        self.0.factory.set(factory).is_ok()
    }
    /// Keep the person's mute choice in `path` from now on, starting from
    /// what it holds.
    pub(crate) fn install_preferences(&self, path: std::path::PathBuf) -> bool {
        self.0.preferences.install(path)
    }
    #[cfg(test)]
    pub(crate) fn preferences(&self) -> &CallPreferences {
        &self.0.preferences
    }
    pub(crate) fn capture_broker(&self) -> Arc<Mutex<CaptureBroker>> {
        Arc::clone(&self.0.capture)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered dispatcher: intent floor, local stops, then the per-command paths"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each block's `entry` borrows from the calls guard up to its return; there is no earlier point to drop it"
    )]
    fn command_for(&self, caller: Caller, request: Request) -> Result<Snapshot, String> {
        if !request.dispatch_valid() {
            return Err("invalid native media intent".into());
        }
        let key = caller.key();
        let intent = request.intent;
        let command = request.command;
        {
            let mut calls = self
                .0
                .calls
                .lock()
                .map_err(|_| "native media unavailable")?;
            if !matches!(command, Command::Status | Command::Watch { .. }) {
                let floor = calls.floors.get(&key).copied().unwrap_or(0);
                if intent <= floor {
                    return Err("stale native media intent".into());
                }
                if !calls.floors.contains_key(&key) && calls.floors.len() >= MAX_CALLS {
                    return Err("native media route capacity reached".into());
                }
                calls.floors.insert(key.clone(), intent);
                if let Some(entry) = calls.entries.get_mut(&key) {
                    entry.intent = intent;
                    if let Some(pending) = entry.pending.take() {
                        pending.task.abort();
                        if let Some(consent) = self.0.consent.get() {
                            consent.prompts.cancel_enqueued_request(&pending.key);
                        }
                    }
                    if entry.backend.is_none() {
                        if let Some(worker) = entry.worker.take() {
                            worker.abort();
                        }
                        entry.snapshot.state = State::Ended;
                    }
                }
            }
            if matches!(command, Command::Leave) {
                if let Some(mut entry) = calls.entries.remove(&key) {
                    self.0.stop_entry(&mut entry);
                }
                return Ok(ended());
            }
            if let Command::SetMuted { muted } = command {
                self.0.preferences.remember(muted);
            }
            if matches!(command, Command::SetMuted { muted: true }) {
                let Some(entry) = calls.entries.get_mut(&key) else {
                    return Ok(idle());
                };
                entry.microphone_on_join = false;
                self.0.stop_capture(entry);
                return Ok(self.0.snapshot(entry));
            }
        }
        (caller.current)()?;
        if self.0.output_disabled.load(Ordering::Acquire) {
            return Ok(Snapshot::unavailable());
        }
        if self.0.factory.get().is_none() || self.0.consent.get().is_none() {
            return Ok(Snapshot::unavailable());
        }
        match command {
            Command::Join { channel_id } => self.join(caller, channel_id, intent),
            Command::Status | Command::Watch { .. } => {
                let calls = self
                    .0
                    .calls
                    .lock()
                    .map_err(|_| "native media unavailable")?;
                Ok(calls
                    .entries
                    .get(&key)
                    .map_or_else(idle, |entry| self.0.snapshot(entry)))
            }
            Command::SetMuted { muted: false } => self.request_capture(&key, intent),
            Command::SelectInput { input_id } => {
                let unmuted = {
                    let mut calls = self
                        .0
                        .calls
                        .lock()
                        .map_err(|_| "native media unavailable")?;
                    let entry = calls
                        .entries
                        .get_mut(&key)
                        .filter(|entry| entry.intent == intent)
                        .ok_or("stale native media intent")?;
                    if !self.0.inputs().iter().any(|input| input.id == input_id) {
                        return Err("input unavailable".into());
                    }
                    let unmuted = !entry.snapshot.muted;
                    self.0.stop_capture(entry);
                    entry.snapshot.input_id = Some(input_id);
                    unmuted
                };
                if unmuted {
                    self.request_capture(&key, intent)
                } else {
                    let calls = self
                        .0
                        .calls
                        .lock()
                        .map_err(|_| "native media unavailable")?;
                    Ok(self
                        .0
                        .snapshot(calls.entries.get(&key).ok_or("no local call")?))
                }
            }
            Command::Leave | Command::SetMuted { muted: true } => {
                Err("native media intent already handled".into())
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the calls guard covers the floor check through worker installation, which must be atomic"
    )]
    fn join(&self, caller: Caller, channel: String, intent: u64) -> Result<Snapshot, String> {
        let permit = self
            .0
            .lifecycle
            .acquire()
            .ok_or("native session inactive")?;
        let target = CallTarget {
            workspace: caller.workspace.clone(),
            app: caller.app.clone(),
            record: caller.record.clone(),
            channel,
        };
        let key = caller.key();
        let mut calls = self
            .0
            .calls
            .lock()
            .map_err(|_| "native media unavailable")?;
        if calls.floors.get(&key) != Some(&intent) {
            return Err("stale native media intent".into());
        }
        if let Some(mut prior) = calls.entries.remove(&key) {
            self.0.stop_entry(&mut prior);
        }
        if calls.entries.len() >= MAX_CALLS {
            return Err("native media capacity reached".into());
        }
        calls.next = calls
            .next
            .checked_add(1)
            .ok_or("native media generation exhausted")?;
        let generation = calls.next;
        let app = VerifiedAppRegistration::new_verified(&caller.app, &caller.workspace)
            .map_err(|e| e.to_string())?;
        let binding = CaptureSessionBinding::from_verified(
            &app,
            format!("native-media-{generation}"),
            self.0.target.as_str(),
        )
        .map_err(|e| e.to_string())?;
        let mut snapshot = idle();
        snapshot.state = State::Joining;
        snapshot.inputs = self.0.inputs();
        snapshot.input_id = snapshot.inputs.first().map(|input| input.id.clone());
        let factory = self
            .0
            .factory
            .get()
            .cloned()
            .ok_or("native media unavailable")?;
        calls.entries.insert(
            key.clone(),
            Entry {
                intent,
                generation,
                caller,
                permit,
                binding,
                snapshot: snapshot.clone(),
                backend: None,
                capture: None,
                pending: None,
                worker: None,
                microphone_on_join: !self.0.preferences.join_muted(),
                unmute_due: false,
                seen: None,
            },
        );
        let weak = Arc::downgrade(&self.0);
        let task_key = key.clone();
        let worker = tokio::spawn(async move {
            run_call(weak, task_key, generation, intent, factory.prepare(target)).await;
        });
        if let Some(entry) = calls.entries.get_mut(&key) {
            entry.worker = Some(worker.abort_handle());
        }
        Ok(snapshot)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "`entry` borrows from the calls guard until the returned snapshot"
    )]
    fn request_capture(&self, key: &Key, intent: u64) -> Result<Snapshot, String> {
        let consent = self.0.consent.get().ok_or("native consent unavailable")?;
        let mut calls = self
            .0
            .calls
            .lock()
            .map_err(|_| "native media unavailable")?;
        let entry = calls
            .entries
            .get_mut(key)
            .filter(|entry| entry.intent == intent)
            .ok_or("stale native media intent")?;
        if !entry.permit.is_current() {
            return Err("local call not ready".into());
        }
        (entry.caller.current)()?;
        if entry.snapshot.state != State::Live {
            // Transport recovery is a call state, not an RPC failure. A
            // refusal would make the client leave an otherwise current call.
            // Do not enqueue consent or remember a future capture request;
            // the user must request unmute again when the call is live.
            tracing::debug!(state = ?entry.snapshot.state, "native unmute requires a live call");
            return Ok(self.0.snapshot(entry));
        }
        if entry
            .capture
            .as_ref()
            .is_some_and(|capture| capture.current.load(Ordering::Acquire))
        {
            return Ok(self.0.snapshot(entry));
        }
        self.0.stop_capture(entry);
        let tree = consent.grants.tree_handle();
        let workspace = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == entry.caller.workspace)
            .map(|workspace| workspace.id)
            .ok_or("local workspace unavailable")?;
        let caller = PortalCaller::new(workspace, entry.caller.app.clone());
        let token = format!("native-media-{}", uuid::Uuid::new_v4());
        // The microphone is asked for once, through the normal permission
        // prompt: an allowance the person gave this app in this room, for the
        // session or always, covers every later join until they take it back.
        let remembered = consent
            .grants
            .lookup(&caller, crate::portal::CapabilityKind::Microphone)
            .is_some_and(|scope| {
                matches!(
                    scope,
                    super::state::GRANT_ALWAYS | super::state::GRANT_SESSION
                )
            });
        let reply = (!remembered).then(|| {
            consent.prompts.enqueue_for_request(
                caller.clone(),
                super::state::Capability::Microphone {
                    scope: super::state::GRANT_ONCE,
                },
                token.clone(),
            )
        });
        let grants = consent.grants.clone();
        let weak = Arc::downgrade(&self.0);
        let task_key = key.clone();
        let task_token = token.clone();
        let generation = entry.generation;
        let task = tokio::spawn(async move {
            let approved = match reply {
                None => true,
                Some(reply) => {
                    let outcome = tokio::time::timeout(CONSENT_TIMEOUT, reply)
                        .await
                        .ok()
                        .and_then(Result::ok);
                    if let Some(scope) = outcome
                        .and_then(crate::portal::PromptOutcome::to_scope)
                        .filter(|scope| {
                            matches!(
                                *scope,
                                super::state::GRANT_ALWAYS | super::state::GRANT_SESSION
                            )
                        })
                    {
                        grants.insert_portal_consent(
                            &caller,
                            &super::state::Capability::Microphone { scope },
                        );
                    }
                    outcome.is_some_and(crate::portal::PromptOutcome::is_allowed)
                }
            };
            if let Some(shared) = weak.upgrade() {
                shared.finish_consent(&task_key, generation, intent, &task_token, approved);
            }
        });
        entry.pending = Some(Pending {
            key: token,
            task: task.abort_handle(),
        });
        entry.snapshot.issue = None;
        Ok(self.0.snapshot(entry))
    }
}
impl NativeMediaService {
    /// Answer once the caller's status differs from the one at `seen`, or
    /// after [`WATCH_HOLD`]: who joins, leaves and talks reaches the app the
    /// moment it happens, with one question outstanding instead of a poll.
    fn watch(
        &self,
        caller: Caller,
        seen: u64,
        route: (String, String, String),
    ) -> NativeMediaFuture {
        let service = self.clone();
        let rosters_changed = self
            .0
            .factory
            .get()
            .and_then(|factory| factory.rosters_changed());
        Box::pin(async move {
            let key = caller.key();
            let start = tokio::time::Instant::now();
            let hold = start.checked_add(WATCH_HOLD).unwrap_or(start);
            let mut refresh = true;
            loop {
                // Both wakers are registered before the status is read, so
                // a change between the read and the wait is not missed.
                let changed = service.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let signal = rosters_changed
                    .as_deref()
                    .map(tokio::sync::Notify::notified);
                tokio::pin!(signal);
                if let Some(signal) = signal.as_mut().as_pin_mut() {
                    signal.enable();
                }
                let rosters = async {
                    match signal.as_mut().as_pin_mut() {
                        Some(signal) => signal.await,
                        None => std::future::pending().await,
                    }
                };
                tokio::pin!(rosters);
                let snapshot =
                    service.command_for(caller.clone(), Request::new(Command::Status))?;
                let snapshot = service.0.finish(snapshot, &key, &route, refresh);
                refresh = false;
                if snapshot.revision != seen || tokio::time::Instant::now() >= hold {
                    return Ok(snapshot);
                }
                tokio::select! {
                    () = &mut changed => {}
                    // The room's calls changed: ask for this record's rosters,
                    // whose arrival wakes `changed`.
                    () = &mut rosters => refresh = true,
                    () = tokio::time::sleep_until(hold) => {}
                }
            }
        })
    }
}
impl NativeMediaServices for NativeMediaService {
    fn command(&self, context: NativeMediaRequestContext, request: Request) -> NativeMediaFuture {
        // Device stop and generation invalidation happen in this synchronous
        // dispatch, before the returned RPC future is polled.
        let command = match &request.command {
            Command::Join { .. } => "join",
            Command::Leave => "leave",
            Command::SetMuted { muted: true } => "mute",
            Command::SetMuted { muted: false } => "unmute",
            Command::SelectInput { .. } => "select_input",
            Command::Status => "status",
            Command::Watch { .. } => "watch",
        };
        let route = (
            context.workspace_id().to_owned(),
            context.app_id().to_owned(),
            context.record_id().to_owned(),
        );
        if let (true, Command::Watch { revision }) = (request.dispatch_valid(), &request.command) {
            return self.watch(context.into(), *revision, route);
        }
        let caller: Caller = context.into();
        let key = caller.key();
        let result = if request.dispatch_valid() {
            self.command_for(caller, request)
        } else {
            Err("invalid native media command".into())
        };
        let result = result.map(|snapshot| self.0.finish(snapshot, &key, &route, true));
        // An effect may have changed what a watcher of this call sees.
        self.0.changed.notify_waiters();
        if let Err(error) = &result {
            let category = match error.as_str() {
                "local call not ready" => "call_not_ready",
                "stale native media intent" => "stale_intent",
                "local media admission ended" => "admission_ended",
                "local media process ended" => "process_ended",
                "local workspace unavailable" => "workspace_unavailable",
                _ => "unavailable",
            };
            tracing::debug!(command, category, "native call local command refused");
        }
        Box::pin(async move { result })
    }
    fn disconnect(&self, peer_id: u64) {
        if let Ok(mut calls) = self.0.calls.lock() {
            calls.floors.retain(|key, _| key.peer != peer_id);
            calls.entries.retain(|key, entry| {
                if key.peer == peer_id {
                    self.0.stop_entry(entry);
                    false
                } else {
                    true
                }
            });
        }
    }
}

impl Shared {
    /// Complete a reply: what a voice channel shows, joined or not (the
    /// rosters last heard for this record, asked again when `refresh`), who
    /// the caller's call hears talking, and the revision a `Watch` waits
    /// past.
    fn finish(
        self: &Arc<Self>,
        mut snapshot: Snapshot,
        key: &Key,
        route: &(String, String, String),
        refresh: bool,
    ) -> Snapshot {
        if snapshot.state != State::Unavailable {
            snapshot.rosters = self.rosters_of(route);
            if refresh {
                self.refresh_rosters(route.clone());
            }
        }
        if snapshot.state == State::Live {
            let backend = self.calls.lock().ok().and_then(|calls| {
                calls
                    .entries
                    .get(key)
                    .and_then(|entry| entry.backend.clone())
            });
            snapshot.speaking = backend
                .and_then(|backend| backend.lock().ok().map(|backend| backend.speaking()))
                .unwrap_or_default();
        }
        snapshot.revision = revision_of(&snapshot);
        snapshot
    }
    fn rosters_of(&self, route: &(String, String, String)) -> Vec<Roster> {
        self.rosters
            .lock_recover()
            .get(route)
            .cloned()
            .unwrap_or_default()
    }
    fn refresh_rosters(self: &Arc<Self>, route: (String, String, String)) {
        let Some(factory) = self.factory.get().cloned() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let rosters = factory.rosters(CallTarget {
            workspace: route.0.clone(),
            app: route.1.clone(),
            record: route.2.clone(),
            channel: String::new(),
        });
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            let rosters = rosters.await;
            if let Some(shared) = weak.upgrade() {
                let before = shared.rosters.lock_recover().insert(route, rosters.clone());
                if before.as_ref() != Some(&rosters) {
                    shared.changed.notify_waiters();
                }
            }
        });
    }
    fn apply_privacy(&self, policy: &crate::portal::lockdown::LockdownPolicy) {
        self.output_disabled
            .store(policy.disable_sound_output, Ordering::Release);
        if let Ok(mut capture) = self.capture.lock() {
            capture.set_kill_switch(CaptureKind::Microphone, policy.disable_microphone);
        }
        if policy.disable_sound_output {
            self.stop_all();
        } else if policy.disable_microphone {
            let mut calls = self.calls.lock_recover();
            for entry in calls.entries.values_mut() {
                self.stop_capture(entry);
            }
        }
    }

    fn inputs(&self) -> Vec<Input> {
        let Ok(capture) = self.capture.lock() else {
            return Vec::new();
        };
        let mut budget = 4096usize;
        capture
            .enumerate_local_nodes(CaptureKind::Microphone)
            .into_iter()
            .take(runtime_collab::native_media::MAX_INPUTS)
            .filter_map(|source| {
                if source.id.is_empty()
                    || source.id.len() > 256
                    || source.id.chars().any(char::is_control)
                {
                    return None;
                }
                let label = source
                    .label
                    .chars()
                    .filter(|c| !c.is_control())
                    .scan(0usize, |bytes, c| {
                        *bytes = bytes.saturating_add(c.len_utf8());
                        (*bytes <= 256).then_some(c)
                    })
                    .collect::<String>();
                if label.is_empty() {
                    return None;
                }
                let input = Input {
                    id: source.id,
                    label,
                };
                let bytes = serde_json::to_vec(&input).ok()?.len();
                budget = budget.checked_sub(bytes)?;
                Some(input)
            })
            .collect()
    }
    fn snapshot(&self, entry: &Entry) -> Snapshot {
        let mut snapshot = entry.snapshot.clone();
        snapshot.inputs = self.inputs();
        snapshot.capture_active = entry
            .capture
            .as_ref()
            .is_some_and(|capture| !capture.paused && (capture.fence)())
            && snapshot.state == State::Live;
        if !entry.permit.is_current() {
            return ended();
        }
        if snapshot.state == State::Live {
            let (inventory, capture_ready) = entry
                .backend
                .as_ref()
                .and_then(|backend| {
                    backend
                        .lock()
                        .ok()
                        .map(|backend| (backend.inventory(), backend.capture_ready()))
                })
                .unwrap_or_default();
            if !inventory.valid() {
                return Snapshot::unavailable();
            }
            snapshot.inventory_revision = inventory.revision;
            snapshot.publication = inventory.publication;
            snapshot.publishers = inventory.publishers;
            snapshot.capture_active &= capture_ready
                && entry.capture.as_ref().is_some_and(|capture| {
                    self.capture.lock().ok().is_some_and(|broker| {
                        matches!(
                            broker.resolve(capture.lease, &entry.binding, Instant::now()),
                            Ok(BorrowedCaptureHandle::PipeWireNode(_))
                        ) && (capture.fence)()
                    })
                });
        }
        snapshot
    }
    fn stop_capture(&self, entry: &mut Entry) {
        if let Some(pending) = entry.pending.take() {
            pending.task.abort();
            if let Some(consent) = self.consent.get() {
                consent.prompts.cancel_enqueued_request(&pending.key);
            }
        }
        if let Some(capture) = entry.capture.take() {
            capture.current.store(false, Ordering::Release);
            if let Some(backend) = &entry.backend {
                backend.lock_recover().stop_capture();
            }
            if let Ok(mut broker) = self.capture.lock() {
                broker.release(capture.lease);
            }
        }
        entry.snapshot.muted = true;
        entry.snapshot.capture_active = false;
    }
    fn stop_entry(&self, entry: &mut Entry) {
        self.stop_capture(entry);
        if let Some(worker) = entry.worker.take() {
            worker.abort();
        }
        if let Some(backend) = entry.backend.take() {
            backend.lock_recover().close();
        }
        if let Ok(mut broker) = self.capture.lock() {
            broker.session_ended(&entry.binding);
        }
    }
    fn stop_all(&self) {
        let mut calls = self.calls.lock_recover();
        for entry in calls.entries.values_mut() {
            self.stop_entry(entry);
        }
        calls.entries.clear();
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one consent completion: authorize, acquire, start and fence under one lifecycle guard"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the broker guard backs the resolved capture handle and must cover lease release on every failure path"
    )]
    fn finish_consent(&self, key: &Key, generation: u64, intent: u64, token: &str, approved: bool) {
        let Ok(mut calls) = self.calls.lock() else {
            return;
        };
        let Some(entry) = calls.entries.get_mut(key).filter(|entry| {
            entry.generation == generation
                && entry.intent == intent
                && entry
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.key == token)
        }) else {
            return;
        };
        entry.pending = None;
        if let Some(consent) = self.consent.get() {
            consent.prompts.cancel_enqueued_request(token);
        }
        if !approved {
            entry.snapshot.issue = Some(Issue::PermissionDenied);
            return;
        }
        if (entry.caller.current)().is_err() || entry.snapshot.state != State::Live {
            entry.snapshot.issue = Some(Issue::Stale);
            return;
        }
        let Some(source) = entry.snapshot.input_id.clone() else {
            entry.snapshot.issue = Some(Issue::SourceLost);
            return;
        };
        let Some(backend) = entry.backend.clone() else {
            return;
        };
        let permit = entry.permit.clone();
        let result = permit
            .while_current(|| -> Result<Capture, Issue> {
                let now = Instant::now();
                let deadline = now
                    .checked_add(crate::portal::capture_broker::CAPTURE_LEASE_TTL)
                    .ok_or(Issue::Unavailable)?;
                let mut broker = self.capture.lock().map_err(|_| Issue::Unavailable)?;
                let auth = broker
                    .authorize(&entry.binding, &source, CaptureKind::Microphone, now)
                    .map_err(|_| Issue::PermissionDenied)?;
                let lease = broker
                    .acquire(auth, &entry.binding, &source, CaptureKind::Microphone, now)
                    .map_err(|_| Issue::PermissionDenied)?;
                let current = Arc::new(AtomicBool::new(true));
                let checked = Arc::clone(&current);
                let fence: Arc<dyn Fn() -> bool + Send + Sync> =
                    Arc::new(move || checked.load(Ordering::Acquire) && Instant::now() < deadline);
                let result = match broker.resolve(lease, &entry.binding, now) {
                    Ok(handle @ BorrowedCaptureHandle::PipeWireNode(_)) => backend
                        .lock()
                        .map_err(|_| Issue::Unavailable)
                        .and_then(|mut backend| {
                            backend.start_capture(&source, handle, fence.clone())
                        }),
                    _ => Err(Issue::SourceLost),
                };
                if let Err(error) = result {
                    current.store(false, Ordering::Release);
                    broker.release(lease);
                    return Err(error);
                }
                let stop = backend
                    .lock()
                    .ok()
                    .and_then(|backend| backend.capture_stop());
                let Some(stop) = stop else {
                    current.store(false, Ordering::Release);
                    backend.lock_recover().stop_capture();
                    broker.release(lease);
                    return Err(Issue::CaptureFailed);
                };
                let stopped = Arc::clone(&current);
                let revoke: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    stopped.store(false, Ordering::Release);
                    stop();
                });
                if broker
                    .stop_on_revoke(lease, &entry.binding, now, revoke.clone())
                    .is_err()
                {
                    revoke();
                    broker.release(lease);
                    return Err(Issue::PermissionDenied);
                }
                Ok(Capture {
                    lease,
                    source,
                    current,
                    fence,
                    paused: false,
                })
            })
            .unwrap_or(Err(Issue::SessionEnded));
        match result {
            Ok(capture) => {
                entry.capture = Some(capture);
                entry.snapshot.muted = false;
                entry.snapshot.issue = None;
            }
            Err(issue) => {
                entry.snapshot.muted = true;
                entry.snapshot.issue = Some(issue);
            }
        }
    }
    fn pause_capture(entry: &mut Entry) {
        let Some(capture) = &mut entry.capture else {
            return;
        };
        if !capture.paused {
            if let Some(backend) = &entry.backend {
                backend.lock_recover().pause_capture();
            }
            capture.paused = true;
        }
        entry.snapshot.capture_active = false;
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the resolved capture handle borrows from the broker guard until the backend installs it"
    )]
    fn resume_capture(&self, entry: &mut Entry) {
        let Some(capture) = entry.capture.as_ref().filter(|capture| capture.paused) else {
            return;
        };
        // Recovery is continuation of this exact explicit unmute, not a new
        // authorization. Re-resolve the existing lease under the same local
        // lifecycle guard before the backend can install a replacement source.
        let result = entry
            .permit
            .while_current(|| -> Result<(), Issue> {
                if !(capture.fence)() || entry.snapshot.input_id.as_ref() != Some(&capture.source) {
                    return Err(Issue::SourceLost);
                }
                (entry.caller.current)().map_err(|_| Issue::SessionEnded)?;
                let broker = self.capture.lock().map_err(|_| Issue::Unavailable)?;
                let handle = broker
                    .resolve(capture.lease, &entry.binding, Instant::now())
                    .map_err(|_| Issue::SourceLost)?;
                if !matches!(handle, BorrowedCaptureHandle::PipeWireNode(_)) {
                    return Err(Issue::SourceLost);
                }
                entry
                    .backend
                    .as_ref()
                    .ok_or(Issue::Unavailable)?
                    .lock()
                    .map_err(|_| Issue::Unavailable)?
                    .start_capture(&capture.source, handle, capture.fence.clone())
            })
            .unwrap_or(Err(Issue::SessionEnded));
        match result {
            Ok(()) => {
                if let Some(capture) = &mut entry.capture {
                    capture.paused = false;
                }
                entry.snapshot.issue = None;
            }
            Err(issue) => {
                self.stop_capture(entry);
                entry.snapshot.issue = Some(issue);
            }
        }
    }
    /// The intent of a join that went live wanting the microphone, once.
    fn take_due_unmute(&self, key: &Key, generation: u64) -> Option<u64> {
        let mut calls = self.calls.lock().ok()?;
        let entry = calls
            .entries
            .get_mut(key)
            .filter(|entry| entry.generation == generation && entry.unmute_due)?;
        entry.unmute_due = false;
        Some(entry.intent)
    }
    fn tick(&self, key: &Key, generation: u64) -> bool {
        // Expiry callbacks stop backend capture while the broker still owns
        // revocation. Never hold a backend mutex while resolving the broker.
        if let Ok(mut broker) = self.capture.lock() {
            broker.reap_expired(Instant::now());
        }
        let Ok(mut calls) = self.calls.lock() else {
            return false;
        };
        let Some(entry) = calls
            .entries
            .get_mut(key)
            .filter(|entry| entry.generation == generation)
        else {
            return false;
        };
        let lifecycle_current = entry.permit.is_current();
        let admission_current = (entry.caller.current)().is_ok();
        if !lifecycle_current || !admission_current {
            tracing::warn!(
                lifecycle_current,
                admission_current,
                "native call local authority ended"
            );
            self.stop_entry(entry);
            calls.entries.remove(key);
            return false;
        }
        if entry
            .capture
            .as_ref()
            .is_some_and(|capture| !capture.current.load(Ordering::Acquire))
        {
            self.stop_capture(entry);
            entry.snapshot.issue = Some(Issue::SourceLost);
        }
        // Async device preparation may complete on any tick. Keep the actual
        // local lifecycle and existing capture lease guarded through this
        // bounded poll/install/send; checking only at start is insufficient.
        // Backend tick must never wait for workers or acquire CaptureBroker.
        let now = Instant::now();
        let result = entry
            .permit
            .while_current(|| -> Result<State, Issue> {
                (entry.caller.current)().map_err(|_| Issue::SessionEnded)?;
                let backend = entry.backend.as_ref().ok_or(Issue::TransportFailed)?;
                if let Some(capture) = &entry.capture {
                    if !(capture.fence)()
                        || entry.snapshot.input_id.as_ref() != Some(&capture.source)
                    {
                        return Err(Issue::SourceLost);
                    }
                    let broker = self.capture.lock().map_err(|_| Issue::Unavailable)?;
                    if !matches!(
                        broker.resolve(capture.lease, &entry.binding, now),
                        Ok(BorrowedCaptureHandle::PipeWireNode(_))
                    ) {
                        return Err(Issue::SourceLost);
                    }
                    let result = backend.lock().map_err(|_| Issue::Unavailable)?.tick(now);
                    drop(broker);
                    result
                } else {
                    backend.lock().map_err(|_| Issue::Unavailable)?.tick(now)
                }
            })
            .unwrap_or(Err(Issue::SessionEnded));
        match result {
            Ok(state) => {
                match state {
                    State::Reconnecting => Self::pause_capture(entry),
                    State::Live => {
                        self.resume_capture(entry);
                        if entry.microphone_on_join
                            && entry.capture.is_none()
                            && entry.pending.is_none()
                        {
                            entry.microphone_on_join = false;
                            entry.unmute_due = true;
                        }
                    }
                    _ => self.stop_capture(entry),
                }
                entry.snapshot.state = state;
                let speaking = entry
                    .backend
                    .as_ref()
                    .and_then(|backend| {
                        backend
                            .lock()
                            .ok()
                            .map(|backend| backend.speaking_revision())
                    })
                    .unwrap_or_default();
                let seen = Some(Seen {
                    state,
                    muted: entry.snapshot.muted,
                    issue: entry.snapshot.issue,
                    capturing: entry.capture.is_some(),
                    asking: entry.pending.is_some(),
                    speaking,
                });
                if entry.seen != seen {
                    entry.seen = seen;
                    self.changed.notify_waiters();
                }
                true
            }
            Err(issue) => {
                self.stop_entry(entry);
                entry.snapshot.state = State::Ended;
                entry.snapshot.issue = Some(issue);
                self.changed.notify_waiters();
                false
            }
        }
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        self.stop_all();
    }
}

async fn run_call(weak: Weak<Shared>, key: Key, generation: u64, intent: u64, prepare: Prepare) {
    let prepared = tokio::time::timeout(Duration::from_secs(10), prepare)
        .await
        .unwrap_or(Err(Issue::TransportFailed));
    let Some(shared) = weak.upgrade() else {
        if let Ok(mut backend) = prepared {
            backend.close();
        }
        return;
    };
    {
        let Ok(mut calls) = shared.calls.lock() else {
            if let Ok(mut backend) = prepared {
                backend.close();
            }
            return;
        };
        let Some(entry) = calls.entries.get_mut(&key).filter(|entry| {
            entry.generation == generation
                && entry.intent == intent
                && entry.permit.is_current()
                && (entry.caller.current)().is_ok()
        }) else {
            if let Ok(mut backend) = prepared {
                backend.close();
            }
            return;
        };
        match prepared {
            Ok(backend) => {
                entry.backend = Some(Arc::new(Mutex::new(backend)));
            }
            Err(issue) => {
                entry.snapshot.state = State::Ended;
                entry.snapshot.issue = Some(issue);
                return;
            }
        }
    }
    drop(shared);
    loop {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        if !shared.tick(&key, generation) {
            return;
        }
        if let Some(intent) = shared.take_due_unmute(&key, generation)
            && let Err(error) =
                NativeMediaService(Arc::clone(&shared)).request_capture(&key, intent)
        {
            tracing::debug!(%error, "the call's microphone did not start on join");
        }
        drop(shared);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
/// The revision of `snapshot`'s content: it changes whenever anything a
/// watcher sees changes. Never zero, which a watcher sends before it has any.
fn revision_of(snapshot: &Snapshot) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&Snapshot {
        revision: 0,
        ..snapshot.clone()
    })
    .unwrap_or_default()
    .hash(&mut hasher);
    hasher.finish().max(1)
}
fn idle() -> Snapshot {
    Snapshot {
        state: State::Idle,
        issue: None,
        ..Snapshot::unavailable()
    }
}
fn ended() -> Snapshot {
    Snapshot {
        state: State::Ended,
        issue: None,
        ..Snapshot::unavailable()
    }
}

#[cfg(test)]
mod audio_tests;
#[cfg(test)]
mod tests;
