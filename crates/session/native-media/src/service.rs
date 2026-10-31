//! The session's call service: which voice channel each app's call is in,
//! who is in the record's channels for it to show, and this device's own
//! call, as presence carries it to everyone in the room.
//!
//! Joining a voice channel is being in its call, muted and listening, with
//! no prompt: nothing is captured until the person asks to be heard, and
//! that is the carrier's to start ([`MediaSession::start_capture`]) after
//! the session asked the person. A call that ends, an app that goes away
//! ([`NativeMediaService::disconnect`]) and a workspace that closes all stop
//! the call and its microphone.
//!
//! Wire intent never grants microphone authority here: a request is taken
//! only above the last intent this route carried, only while the app's
//! admitted connection is live, and only for its own call.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use runtime_collab::native_media::{
    Command, Issue, PICTURES_HOLD, Request, Roster, Snapshot, State, WATCH_HOLD,
};
use tilo_action_broker::services::{
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
};
use tilo_lock::MutexRecover as _;

use crate::media::{CallTarget, MediaFactory, MediaSession, Prepare};

/// The most app records one device holds calls for at once.
const MAX_CALLS: usize = 16;
/// How long a call's carrier may take to prepare it before it is over.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a call's own state is asked of its carrier: what keeps a
/// watcher current and ends a call whose carrier is gone. Nothing is
/// captured on this beat; a person's commands and the carrier's own events
/// move a call, not the tick.
const TICK: Duration = Duration::from_millis(250);

type Backend = Arc<Mutex<Box<dyn MediaSession>>>;
type Revalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// An app's admitted connection, as its call is keyed and fenced by it.
#[derive(Clone)]
pub struct Caller {
    peer: u64,
    app: String,
    workspace: String,
    record: String,
    current: Revalidate,
}

impl Caller {
    /// A connection the host admitted itself, in its own process: the id it
    /// keeps for the connection, the app, the workspace the app runs in, the
    /// record it shows, and how to tell the connection is still there.
    #[must_use]
    pub fn connected(
        peer: u64,
        app: &str,
        workspace: &str,
        record: &str,
        current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Self {
        Self {
            peer,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            record: record.to_owned(),
            current,
        }
    }
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

struct Entry {
    /// The last join or microphone intent this route carried.
    intent: u64,
    generation: u64,
    caller: Caller,
    /// The voice channel of the caller's record this call is in.
    channel: String,
    snapshot: Snapshot,
    backend: Option<Backend>,
    worker: Option<tokio::task::AbortHandle>,
    /// What the last tick saw of the call, to wake watchers when it changes.
    seen: Option<Seen>,
    /// Whether the microphone was muted when the person deafened: what
    /// undeafening puts back.
    muted_before_deafen: bool,
}

/// What a watcher of a call sees change between ticks.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    state: State,
    issue: Option<Issue>,
    muted: bool,
    capturing: bool,
}

impl Seen {
    fn of(entry: &Entry) -> Self {
        Self {
            state: entry.snapshot.state,
            issue: entry.snapshot.issue,
            muted: entry.snapshot.muted,
            capturing: entry.snapshot.capture_active,
        }
    }
}

#[derive(Default)]
struct Calls {
    next: u64,
    entries: BTreeMap<Key, Entry>,
    floors: BTreeMap<Key, u64>,
}

struct Shared {
    factory: OnceLock<Arc<dyn MediaFactory>>,
    calls: Mutex<Calls>,
    /// The calls of each caller's record, as last heard.
    rosters: Mutex<BTreeMap<(String, String, String), Vec<Roster>>>,
    /// Woken whenever a status may have changed: what a `Watch` waits on.
    changed: tokio::sync::Notify,
}

/// The voice channel this device is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCall {
    /// The workspace (its room id) the call is in.
    pub workspace: String,
    /// The app the call belongs to.
    pub app: String,
    /// The record of that app the voice channels are in.
    pub record: String,
    /// The voice channel of that app.
    pub channel: String,
}

/// The session's calls, as the apps it runs reach them.
#[derive(Clone)]
pub struct NativeMediaService(Arc<Shared>);

impl Default for NativeMediaService {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeMediaService {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Shared {
            factory: OnceLock::new(),
            calls: Mutex::new(Calls::default()),
            rosters: Mutex::new(BTreeMap::new()),
            changed: tokio::sync::Notify::new(),
        }))
    }

    /// Hand the calls their carrier: what joins a channel, and who is in
    /// the record's channels. Without one, no call can start.
    pub fn install_factory(&self, factory: Arc<dyn MediaFactory>) -> bool {
        self.0.factory.set(factory).is_ok()
    }

    /// The voice channel this device is in, or on its way into: one at a
    /// time, which is what presence carries.
    #[must_use]
    pub fn own_call(&self) -> Option<OwnCall> {
        let calls = self.0.calls.lock().ok()?;
        calls
            .entries
            .values()
            .find(|entry| {
                matches!(
                    entry.snapshot.state,
                    State::Joining | State::Live | State::Reconnecting
                )
            })
            .map(|entry| OwnCall {
                workspace: entry.caller.workspace.clone(),
                app: entry.caller.app.clone(),
                record: entry.caller.record.clone(),
                channel: entry.channel.clone(),
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered dispatcher: intent floor, local stops, then the per-command paths"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each block's `entry` borrows from the calls guard up to its return; there is no earlier point to drop it"
    )]
    /// Run one command for `caller` and answer with the call as it stands:
    /// what the broker dispatches when the app's own connection sends one.
    pub fn command_for(&self, caller: Caller, request: Request) -> Result<Snapshot, String> {
        if !request.dispatch_valid() {
            return Err("invalid native media intent".into());
        }
        let key = caller.key();
        let intent = request.intent;
        let carries_intent = request.carries_intent();
        let command = request.command;
        // The microphone's prompt answers only the microphone's commands: a
        // camera turned on meanwhile does not cancel it.
        let microphone = matches!(
            command,
            Command::Join { .. }
                | Command::SetMuted { .. }
                | Command::SetDeafened { .. }
                | Command::SelectInput { .. }
                | Command::Leave
        );
        {
            let mut calls = self
                .0
                .calls
                .lock()
                .map_err(|_| "native media unavailable")?;
            if carries_intent {
                let floor = calls.floors.get(&key).copied().unwrap_or(0);
                if intent <= floor {
                    return Err("stale native media intent".into());
                }
                if !calls.floors.contains_key(&key) && calls.floors.len() >= MAX_CALLS {
                    return Err("native media route capacity reached".into());
                }
                calls.floors.insert(key.clone(), intent);
                if let Some(entry) = calls.entries.get_mut(&key) {
                    if microphone {
                        entry.intent = intent;
                    }
                    if entry.backend.is_none() && microphone {
                        if let Some(worker) = entry.worker.take() {
                            worker.abort();
                        }
                        entry.snapshot.state = State::Ended;
                    }
                }
            }
            if matches!(command, Command::Leave) {
                if let Some(mut entry) = calls.entries.remove(&key) {
                    Shared::stop_entry(&mut entry);
                }
                return Ok(ended());
            }
            if matches!(command, Command::SetMuted { muted: true }) {
                let Some(entry) = calls.entries.get_mut(&key) else {
                    return Ok(idle());
                };
                Shared::stop_capture(entry);
                return Ok(Shared::snapshot(entry));
            }
            // Deafening silences the call and mutes at once; undeafening
            // hears it at once and unmutes below when it was unmuted before,
            // as unmuting while deafened hears the call again.
            if let Command::SetDeafened { deafened } = command {
                let Some(entry) = calls.entries.get_mut(&key) else {
                    return Ok(idle());
                };
                if deafened {
                    if !entry.snapshot.deafened {
                        entry.muted_before_deafen = entry.snapshot.muted;
                    }
                    entry.snapshot.deafened = true;
                    Shared::stop_capture(entry);
                    return Ok(Shared::snapshot(entry));
                }
                let was = entry.snapshot.deafened;
                entry.snapshot.deafened = false;
                if !was || entry.muted_before_deafen {
                    return Ok(Shared::snapshot(entry));
                }
            }
            if matches!(command, Command::SetMuted { muted: false })
                && let Some(entry) = calls
                    .entries
                    .get_mut(&key)
                    .filter(|entry| entry.snapshot.deafened)
            {
                entry.snapshot.deafened = false;
            }
        }
        (caller.current)()?;
        if self.0.factory.get().is_none() {
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
                Ok(calls.entries.get(&key).map_or_else(idle, Shared::snapshot))
            }
            Command::SetMuted { muted: false } | Command::SetDeafened { deafened: false } => {
                self.unmute(&key, intent)
            }
            // A call carries no camera or screen here yet: this device says
            // so instead of showing a picture nobody receives.
            Command::SetCamera { on: true } => self.unavailable_source(&key, Issue::CameraLost),
            Command::SetScreen { on: true } => self.unavailable_source(&key, Issue::ScreenLost),
            Command::SetVolume { .. }
            | Command::Show { .. }
            | Command::SetCamera { on: false }
            | Command::SetScreen { on: false } => {
                let calls = self
                    .0
                    .calls
                    .lock()
                    .map_err(|_| "native media unavailable")?;
                Ok(calls.entries.get(&key).map_or_else(idle, Shared::snapshot))
            }
            Command::SelectInput { input_id } => {
                let mut calls = self
                    .0
                    .calls
                    .lock()
                    .map_err(|_| "native media unavailable")?;
                let Some(entry) = calls
                    .entries
                    .get_mut(&key)
                    .filter(|entry| entry.intent == intent)
                else {
                    return Err("stale native media intent".into());
                };
                (entry.caller.current)()?;
                if !entry
                    .snapshot
                    .inputs
                    .iter()
                    .any(|input| input.id == input_id)
                {
                    return Err("input unavailable".into());
                }
                let unmuted = !entry.snapshot.muted;
                Shared::stop_capture(entry);
                entry.snapshot.input_id = Some(input_id);
                drop(calls);
                if unmuted {
                    self.unmute(&key, intent)
                } else {
                    let calls = self
                        .0
                        .calls
                        .lock()
                        .map_err(|_| "native media unavailable")?;
                    Ok(Shared::snapshot(
                        calls.entries.get(&key).ok_or("no local call")?,
                    ))
                }
            }
            Command::Leave
            | Command::SetMuted { muted: true }
            | Command::SetDeafened { deafened: true } => {
                Err("native media intent already handled".into())
            }
            Command::Pictures { .. } => Err("pictures are collected on their own lane".into()),
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the calls guard covers the floor check through worker installation, which must be atomic"
    )]
    fn join(&self, caller: Caller, channel: String, intent: u64) -> Result<Snapshot, String> {
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
            Shared::stop_entry(&mut prior);
        }
        if calls.entries.len() >= MAX_CALLS {
            return Err("native media capacity reached".into());
        }
        calls.next = calls
            .next
            .checked_add(1)
            .ok_or("native media generation exhausted")?;
        let generation = calls.next;
        let mut snapshot = idle();
        snapshot.state = State::Joining;
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
                channel: target.channel.clone(),
                caller,
                snapshot: snapshot.clone(),
                backend: None,
                worker: None,
                seen: None,
                muted_before_deafen: true,
            },
        );
        let weak = Arc::downgrade(&self.0);
        let task_key = key.clone();
        let prepare: Prepare = factory.prepare(target);
        let worker = tokio::spawn(async move {
            run_call(weak, task_key, generation, intent, prepare).await;
        });
        if let Some(entry) = calls.entries.get_mut(&key) {
            entry.worker = Some(worker.abort_handle());
        }
        Ok(snapshot)
    }

    /// Ask the carrier to send this person's microphone: only in a live
    /// call, only from the input they chose, and only once the session asked
    /// the person (the carrier's `start_capture` is that authority).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`entry` borrows from the calls guard until the returned snapshot"
    )]
    fn unmute(&self, key: &Key, intent: u64) -> Result<Snapshot, String> {
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
        (entry.caller.current)()?;
        if entry.snapshot.state != State::Live {
            // Transport recovery is a call state, not an RPC failure. A
            // refusal would make the client leave an otherwise current call.
            // The person asks to be heard again once the call is live.
            tracing::debug!(
                state = ?entry.snapshot.state,
                "native unmute requires a live call"
            );
            return Ok(Shared::snapshot(entry));
        }
        if !entry.snapshot.muted {
            return Ok(Shared::snapshot(entry));
        }
        let Some(backend) = entry.backend.clone() else {
            return Ok(Shared::snapshot(entry));
        };
        entry.snapshot.inputs = backend.lock_recover().inputs();
        let Some(input) = entry
            .snapshot
            .input_id
            .clone()
            .or_else(|| entry.snapshot.inputs.first().map(|input| input.id.clone()))
        else {
            entry.snapshot.issue = Some(Issue::SourceLost);
            return Ok(Shared::snapshot(entry));
        };
        entry.snapshot.input_id = Some(input.clone());
        let captured = backend.lock_recover().start_capture(&input);
        match captured {
            Ok(()) => {
                entry.snapshot.muted = false;
                entry.snapshot.capture_active = true;
                entry.snapshot.issue = None;
            }
            Err(issue) => {
                entry.snapshot.muted = true;
                entry.snapshot.capture_active = false;
                entry.snapshot.issue = Some(issue);
            }
        }
        Ok(Shared::snapshot(entry))
    }

    /// A camera or screen this call cannot publish: say why, and change
    /// nothing else.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`entry` borrows from the calls guard until the returned snapshot"
    )]
    fn unavailable_source(&self, key: &Key, issue: Issue) -> Result<Snapshot, String> {
        let mut calls = self
            .0
            .calls
            .lock()
            .map_err(|_| "native media unavailable")?;
        let Some(entry) = calls.entries.get_mut(key) else {
            return Ok(idle());
        };
        entry.snapshot.issue = Some(issue);
        Ok(Shared::snapshot(entry))
    }

    /// Answer once the caller's status differs from the one at `seen`, or
    /// after [`WATCH_HOLD`]: who joins and leaves reaches the app the moment
    /// it happens, with one question outstanding instead of a poll.
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
                let snapshot = service.0.finish(snapshot, &route, refresh);
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

    /// Hand the caller the pictures that arrived after `after`, as soon as
    /// there are any or after [`PICTURES_HOLD`]: a call with no carrier
    /// sends none, so the answer is the call's state and no pictures.
    fn pictures(&self, caller: Caller, after: u64) -> NativeMediaFuture {
        let service = self.clone();
        Box::pin(async move {
            let key = caller.key();
            let start = tokio::time::Instant::now();
            let hold = start.checked_add(PICTURES_HOLD).unwrap_or(start);
            loop {
                (caller.current)()?;
                let changed = service.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let state = service
                    .0
                    .calls
                    .lock()
                    .map_err(|_| "native media unavailable")?
                    .entries
                    .get(&key)
                    .map_or(State::Idle, |entry| entry.snapshot.state);
                if matches!(state, State::Idle | State::Ended | State::Unavailable)
                    || tokio::time::Instant::now() >= hold
                {
                    return Ok(Snapshot {
                        state,
                        issue: None,
                        pictures_cursor: after,
                        ..idle()
                    });
                }
                tokio::select! {
                    () = &mut changed => {}
                    () = tokio::time::sleep_until(hold) => {}
                }
            }
        })
    }
}

impl NativeMediaService {
    /// Run one command for `caller`: its answer carries the rosters this
    /// record last heard, and a watcher of this call is woken even when the
    /// command changed nothing.
    fn dispatch(
        &self,
        caller: Caller,
        request: Request,
        route: &(String, String, String),
    ) -> Result<Snapshot, String> {
        let result = if request.dispatch_valid() {
            self.command_for(caller, request)
        } else {
            Err("invalid native media command".into())
        };
        let result = result.map(|snapshot| self.0.finish(snapshot, route, true));
        // An effect may have changed what a watcher of this call sees.
        self.0.changed.notify_waiters();
        if let Err(error) = &result {
            let category = match error.as_str() {
                "stale native media intent" => "stale_intent",
                _ => "unavailable",
            };
            tracing::debug!(category, "native call local command refused");
        }
        result
    }
}

impl NativeMediaServices for NativeMediaService {
    fn command(&self, context: NativeMediaRequestContext, request: Request) -> NativeMediaFuture {
        // Device stop and generation invalidation happen in this synchronous
        // dispatch, before the returned RPC future is polled.
        let route = (
            context.workspace_id().to_owned(),
            context.app_id().to_owned(),
            context.record_id().to_owned(),
        );
        if let (true, Command::Watch { revision }) = (request.dispatch_valid(), &request.command) {
            return self.watch(context.into(), *revision, route);
        }
        if let (true, Command::Pictures { after }) = (request.dispatch_valid(), &request.command) {
            return self.pictures(context.into(), *after);
        }
        let result = self.dispatch(context.into(), request, &route);
        Box::pin(async move { result })
    }

    fn disconnect(&self, peer_id: u64) {
        let mut calls = self.0.calls.lock_recover();
        calls.floors.retain(|key, _| key.peer != peer_id);
        calls.entries.retain(|key, entry| {
            if key.peer == peer_id {
                Shared::stop_entry(entry);
                false
            } else {
                true
            }
        });
    }
}

impl Shared {
    /// Complete a reply: what a voice channel shows, joined or not (the
    /// rosters last heard for this record, asked again when `refresh`), and
    /// the revision a `Watch` waits past.
    fn finish(
        self: &Arc<Self>,
        mut snapshot: Snapshot,
        route: &(String, String, String),
        refresh: bool,
    ) -> Snapshot {
        if snapshot.state != State::Unavailable {
            snapshot.rosters = self.rosters_of(route);
            if refresh {
                self.refresh_rosters(route.clone());
            }
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

    fn snapshot(entry: &Entry) -> Snapshot {
        let mut snapshot = entry.snapshot.clone();
        // What this person shows: only within a call.
        let in_call = !matches!(
            snapshot.state,
            State::Unavailable | State::Idle | State::Ended
        );
        snapshot.deafened &= in_call;
        snapshot.capture_active &= snapshot.state == State::Live && !snapshot.muted;
        if snapshot.state == State::Live
            && let Some(backend) = &entry.backend
        {
            let inventory = {
                let backend = backend.lock_recover();
                snapshot.inputs = backend.inputs();
                backend.inventory()
            };
            if !inventory.valid() {
                return Snapshot::unavailable();
            }
            snapshot.inventory_revision = inventory.revision;
            snapshot.publication = inventory.publication;
            snapshot.publishers = inventory.publishers;
        }
        snapshot
    }

    fn stop_capture(entry: &mut Entry) {
        if let Some(backend) = &entry.backend {
            backend.lock_recover().stop_capture();
        }
        entry.snapshot.muted = true;
        entry.snapshot.capture_active = false;
    }

    fn stop_entry(entry: &mut Entry) {
        Shared::stop_capture(entry);
        if let Some(worker) = entry.worker.take() {
            worker.abort();
        }
        if let Some(backend) = entry.backend.take() {
            backend.lock_recover().close();
        }
    }

    fn tick(&self, key: &Key, generation: u64) -> bool {
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
        if (entry.caller.current)().is_err() {
            tracing::debug!("native call's app connection ended");
            Shared::stop_entry(entry);
            calls.entries.remove(key);
            return false;
        }
        let backend = entry.backend.clone();
        let now = Instant::now();
        let result = match backend {
            Some(backend) => backend.lock_recover().tick(now),
            None => Err(Issue::TransportFailed),
        };
        match result {
            Ok(state) => {
                if state != State::Live && state != State::Reconnecting {
                    Shared::stop_capture(entry);
                }
                entry.snapshot.state = state;
                let seen = Some(Seen::of(entry));
                if entry.seen != seen {
                    entry.seen = seen;
                    self.changed.notify_waiters();
                }
                true
            }
            Err(issue) => {
                Shared::stop_entry(entry);
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
        if let Ok(calls) = self.calls.get_mut() {
            for entry in calls.entries.values_mut() {
                if let Some(backend) = &entry.backend {
                    backend.lock_recover().stop_capture();
                }
                if let Some(backend) = entry.backend.take() {
                    backend.lock_recover().close();
                }
            }
        }
    }
}

async fn run_call(weak: WeakShared, key: Key, generation: u64, intent: u64, prepare: Prepare) {
    let prepared = tokio::time::timeout(PREPARE_TIMEOUT, prepare)
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
                && (entry.caller.current)().is_ok()
        }) else {
            if let Ok(mut backend) = prepared {
                backend.close();
            }
            return;
        };
        match prepared {
            Ok(backend) => entry.backend = Some(Arc::new(Mutex::new(backend))),
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
        drop(shared);
        tokio::time::sleep(TICK).await;
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

type WeakShared = std::sync::Weak<Shared>;

#[cfg(test)]
mod tests;
