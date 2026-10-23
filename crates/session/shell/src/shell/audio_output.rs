//! Device-local, launcher-attested app PCM. Output ownership and capture authority are separate.
use super::collab_broker_capnp::{LocalSessionLifecycle, LocalSessionPermit};
use media::pw_stream::{PlaybackSource, PlaybackStream};
use runtime_collab::audio_output::{CHANNELS, Command, MAX_SAMPLES, Snapshot};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use tilo_action_broker::services::{
    AudioOutputFuture, AudioOutputServices, NativeMediaRequestContext,
};

const MAX_OUTPUTS: usize = 8;
const MAX_QUEUED_SAMPLES: usize = MAX_SAMPLES * 8;
/// How often an open output's admission is re-checked. Admission has a clock
/// in it (certificate expiry), so while an output is open this stays a timer.
const REVALIDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
type Prepare = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Box<dyn PlaybackStream>, String>> + Send>,
>;
type Factory = dyn Fn(String, Box<dyn PlaybackSource>) -> Prepare + Send + Sync;
type PrivacyListener = dyn Fn(&crate::portal::lockdown::LockdownPolicy) + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    peer: u64,
    app: String,
    workspace: String,
    record: String,
}
impl From<&NativeMediaRequestContext> for Key {
    fn from(context: &NativeMediaRequestContext) -> Self {
        Self {
            peer: context.peer_id(),
            app: context.app_id().into(),
            workspace: context.workspace_id().into(),
            record: context.record_id().into(),
        }
    }
}
#[derive(Clone)]
struct Caller {
    key: Key,
    current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
impl From<NativeMediaRequestContext> for Caller {
    fn from(context: NativeMediaRequestContext) -> Self {
        Self {
            key: Key::from(&context),
            current: Arc::new(move || context.revalidate()),
        }
    }
}

struct Pcm {
    queue: VecDeque<i16>,
    paused: bool,
    gain: f32,
    rendered: u64,
    sequence: u64,
    last_digest: Option<blake3::Hash>,
}
impl Default for Pcm {
    fn default() -> Self {
        Self {
            queue: VecDeque::with_capacity(MAX_QUEUED_SAMPLES),
            paused: false,
            gain: 1.0,
            rendered: 0,
            sequence: 0,
            last_digest: None,
        }
    }
}
struct Source {
    pcm: Arc<Mutex<Pcm>>,
    alive: Arc<AtomicBool>,
}
impl PlaybackSource for Source {
    fn render(&mut self, _presentation_ns: i64, out: &mut [f32]) {
        out.fill(0.0);
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut pcm) = self.pcm.try_lock() else {
            return;
        };
        if pcm.paused {
            return;
        }
        let mut rendered = 0_u64;
        for sample in out {
            let Some(value) = pcm.queue.pop_front() else {
                break;
            };
            *sample = f32::from(value) / 32768.0 * pcm.gain;
            rendered = rendered.saturating_add(1);
        }
        pcm.rendered = pcm.rendered.saturating_add(
            rendered
                .checked_div(u64::from(CHANNELS))
                .unwrap_or_default(),
        );
    }
}
struct Entry {
    generation: u64,
    permit: LocalSessionPermit,
    caller: Caller,
    pcm: Arc<Mutex<Pcm>>,
    alive: Arc<AtomicBool>,
    stream: Option<Box<dyn PlaybackStream>>,
}
impl Entry {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let pcm = self
            .pcm
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        if let Some(failure) = self.stream.as_ref().and_then(|stream| stream.failure()) {
            return Err(failure);
        }
        Ok(Snapshot {
            generation: self.generation,
            accepted_sequence: pcm.sequence,
            rendered_frames: pcm.rendered,
            queued_frames: u32::try_from(
                pcm.queue
                    .len()
                    .checked_div(usize::from(CHANNELS))
                    .unwrap_or_default(),
            )
            .unwrap_or(u32::MAX),
            paused: pcm.paused,
            ready: self.alive.load(Ordering::Acquire)
                && self
                    .stream
                    .as_ref()
                    .is_some_and(|stream| stream.timing().is_some()),
            closed: !self.alive.load(Ordering::Acquire),
        })
    }
    fn fence(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut pcm) = self.pcm.lock() {
            pcm.queue.clear();
        }
        if let Some(stream) = self.stream.take() {
            // The process callback is fenced first. PipeWire joins never run on the compositor/RPC lane.
            let _ = std::thread::Builder::new()
                .name("tilo-audio-output-retire".into())
                .spawn(move || drop(stream));
        }
    }
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.fence();
    }
}
#[derive(Default)]
struct Outputs {
    next: u64,
    entries: BTreeMap<Key, Entry>,
}
/// Wakes the authority watchdog when there is something to watch again.
///
/// With no output open there is no admission to re-check, so the watchdog
/// blocks here instead of ticking: an idle desktop's audio
/// authority costs no wakeups.
#[derive(Default)]
struct Watch {
    state: Mutex<WatchState>,
    wake: Condvar,
}
#[derive(Default)]
struct WatchState {
    /// Bumped by every `open`; read before the watchdog looks at the
    /// registry, so an output inserted after that look still wakes it.
    epoch: u64,
    /// The watchdog is blocked in [`Watch::wait_after`].
    parked: bool,
    /// The service is gone; the watchdog exits.
    closed: bool,
}
impl Watch {
    /// The current epoch, or `None` once the service closed.
    fn epoch(&self) -> Option<u64> {
        let state = self.state.lock().ok()?;
        (!state.closed).then_some(state.epoch)
    }
    fn notify(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.epoch = state.epoch.wrapping_add(1);
            if state.parked {
                self.wake.notify_all();
            }
        }
    }
    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            self.wake.notify_all();
        }
    }
    /// Blocks, without a timeout, until the epoch moves past `seen` or the
    /// service closes.
    fn wait_after(&self, seen: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.parked = true;
        while state.epoch == seen && !state.closed {
            state = match self.wake.wait(state) {
                Ok(state) => state,
                Err(_) => return,
            };
        }
        state.parked = false;
    }
}
struct Shared {
    lifecycle: LocalSessionLifecycle,
    outputs: Mutex<Outputs>,
    disabled: AtomicBool,
    factory: Arc<Factory>,
    lifecycle_listener: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    privacy_listener: Mutex<Option<Arc<PrivacyListener>>>,
    watch: Arc<Watch>,
}
impl Drop for Shared {
    fn drop(&mut self) {
        self.watch.close();
    }
}
impl Shared {
    fn watching(&self) -> bool {
        self.outputs
            .lock()
            .is_ok_and(|outputs| !outputs.entries.is_empty())
    }
    fn revalidate(&self) {
        let callers = self
            .outputs
            .lock()
            .map(|outputs| {
                outputs
                    .entries
                    .iter()
                    .map(|(key, entry)| (key.clone(), entry.generation, entry.caller.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (key, generation, caller) in callers {
            if (caller.current)().is_err()
                && let Ok(mut outputs) = self.outputs.lock()
                && outputs
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.generation == generation)
            {
                outputs.entries.remove(&key);
            }
        }
    }
    fn stop(&self, peer: Option<u64>) {
        if let Ok(mut outputs) = self.outputs.lock() {
            outputs.entries.retain(|key, entry| {
                if peer.is_none_or(|peer| key.peer == peer) {
                    entry.fence();
                    false
                } else {
                    true
                }
            });
        }
    }
}
#[derive(Clone)]
pub(crate) struct AudioOutputService(Arc<Shared>);
impl AudioOutputService {
    pub(crate) fn new(lifecycle: LocalSessionLifecycle) -> Self {
        Self::with_factory(
            lifecycle,
            Arc::new(|app, source| {
                Box::pin(crate::network::native_media::prepare_playback(app, source))
            }),
        )
    }
    fn with_factory(lifecycle: LocalSessionLifecycle, factory: Arc<Factory>) -> Self {
        let shared = Arc::new(Shared {
            lifecycle,
            outputs: Mutex::new(Outputs::default()),
            disabled: AtomicBool::new(false),
            factory,
            lifecycle_listener: Mutex::new(None),
            privacy_listener: Mutex::new(None),
            watch: Arc::default(),
        });
        let weak = Arc::downgrade(&shared);
        let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.stop(None);
            }
        });
        shared.lifecycle.watch(&listener);
        if let Ok(mut keep) = shared.lifecycle_listener.lock() {
            *keep = Some(listener);
        }
        let weak = Arc::downgrade(&shared);
        let privacy: Arc<PrivacyListener> = Arc::new(move |policy| {
            if let Some(shared) = weak.upgrade() {
                shared
                    .disabled
                    .store(policy.disable_sound_output, Ordering::Release);
                if policy.disable_sound_output {
                    shared.stop(None);
                }
            }
        });
        if crate::portal::lockdown::watch_policy(&privacy).is_err() {
            shared.disabled.store(true, Ordering::Release);
        }
        if let Ok(mut keep) = shared.privacy_listener.lock() {
            *keep = Some(privacy);
        }
        let weak = Arc::downgrade(&shared);
        let watch = shared.watch.clone();
        if std::thread::Builder::new()
            .name("tilo-audio-output-authority".into())
            .spawn(move || {
                // Re-check every open output's admission on a short timer;
                // with none open, block until `open` registers one.
                while let Some(seen) = watch.epoch() {
                    let Some(shared) = weak.upgrade() else {
                        break;
                    };
                    let watching = shared.watching();
                    if watching {
                        shared.revalidate();
                    }
                    drop(shared);
                    if watching {
                        std::thread::sleep(REVALIDATE_INTERVAL);
                    } else {
                        watch.wait_after(seen);
                    }
                }
            })
            .is_err()
        {
            shared.disabled.store(true, Ordering::Release);
        }
        Self(shared)
    }

    async fn open(&self, caller: Caller) -> Result<Snapshot, String> {
        (caller.current)()?;
        if self.0.disabled.load(Ordering::Acquire) {
            return Err("Sound output is disabled".into());
        }
        let permit = self
            .0
            .lifecycle
            .acquire()
            .ok_or("Local session is inactive")?;
        let key = caller.key.clone();
        let pcm = Arc::new(Mutex::new(Pcm::default()));
        let alive = Arc::new(AtomicBool::new(true));
        let generation = {
            let mut outputs = self
                .0
                .outputs
                .lock()
                .map_err(|_| "Audio output state unavailable")?;
            if !outputs.entries.contains_key(&key) && outputs.entries.len() >= MAX_OUTPUTS {
                return Err("Audio output capacity reached".into());
            }
            let generation = outputs
                .next
                .checked_add(1)
                .ok_or("Audio output generation exhausted")?;
            outputs.next = generation;
            outputs.entries.insert(
                key.clone(),
                Entry {
                    generation,
                    permit: permit.clone(),
                    caller: caller.clone(),
                    pcm: pcm.clone(),
                    alive: alive.clone(),
                    stream: None,
                },
            );
            generation
        };
        // Revalidation covers the preparation window too.
        self.0.watch.notify();
        let mut guard = PreparationGuard {
            shared: Arc::downgrade(&self.0),
            key: key.clone(),
            generation,
            armed: true,
        };
        let stream = (self.0.factory)(key.app.clone(), Box::new(Source { pcm, alive })).await?;
        (caller.current)()?;
        let mut outputs = self
            .0
            .outputs
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        let entry = outputs
            .entries
            .get_mut(&key)
            .filter(|entry| entry.generation == generation)
            .ok_or("Audio output was superseded")?;
        if self.0.disabled.load(Ordering::Acquire) || !permit.is_current() {
            return Err("Local audio output permission ended".into());
        }
        entry.stream = Some(stream);
        let snapshot = entry.snapshot()?;
        drop(outputs);
        guard.armed = false;
        Ok(snapshot)
    }

    fn revalidate_command(caller: &Caller, stage: &str) -> Result<(), String> {
        let started = std::time::Instant::now();
        let result = (caller.current)();
        let elapsed = started.elapsed();
        if elapsed >= std::time::Duration::from_millis(50) {
            tracing::warn!(
                target: "tilo::audio_output",
                stage,
                elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                refused = result.is_err(),
                "governed audio admission check stalled"
            );
        }
        result
    }

    fn entry_caller(&self, key: &Key, generation: u64) -> Result<Caller, String> {
        let outputs = self
            .0
            .outputs
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        outputs
            .entries
            .get(key)
            .filter(|entry| entry.generation == generation)
            .map(|entry| entry.caller.clone())
            .ok_or_else(|| "Stale audio output generation".into())
    }

    fn control(&self, caller: Caller, command: Command) -> Result<Snapshot, String> {
        if let Err(error) = Self::revalidate_command(&caller, "command") {
            self.0.stop(Some(caller.key.peer));
            return Err(error);
        }
        let key = caller.key;
        let generation = match &command {
            Command::Open {} => return Err("Audio output must be opened asynchronously".into()),
            Command::Write { generation, .. }
            | Command::Pause { generation, .. }
            | Command::Volume { generation, .. }
            | Command::Flush { generation }
            | Command::Status { generation }
            | Command::Close { generation } => *generation,
        };
        // Admission can wait on membership, identity or process custody.
        // Never carry the output registry lock into that external callback:
        // session locking must still fence the stream while it is waiting.
        let retained_caller = self.entry_caller(&key, generation)?;
        let admission = Self::revalidate_command(&retained_caller, "retained-output");
        let mut outputs = self
            .0
            .outputs
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        let entry = outputs
            .entries
            .get_mut(&key)
            .filter(|entry| entry.generation == generation)
            .ok_or("Stale audio output generation")?;
        if !entry.permit.is_current() || self.0.disabled.load(Ordering::Acquire) {
            entry.fence();
            return Err("Local audio output permission ended".into());
        }
        if let Err(error) = admission {
            entry.fence();
            return Err(error);
        }
        match command {
            Command::Close { .. } => {
                entry.fence();
                let snapshot = entry.snapshot()?;
                outputs.entries.remove(&key);
                drop(outputs);
                return Ok(snapshot);
            }
            Command::Status { .. } => {}
            Command::Write {
                sequence, samples, ..
            } => Self::write(entry, sequence, samples)?,
            Command::Pause { paused, .. } => {
                entry
                    .pcm
                    .lock()
                    .map_err(|_| "Audio output state unavailable")?
                    .paused = paused;
            }
            Command::Volume { percent, .. } => {
                entry
                    .pcm
                    .lock()
                    .map_err(|_| "Audio output state unavailable")?
                    .gain = f32::from(percent) / 100.0;
            }
            Command::Flush { .. } => entry
                .pcm
                .lock()
                .map_err(|_| "Audio output state unavailable")?
                .queue
                .clear(),
            Command::Open {} => return Err("Invalid audio output command".into()),
        }
        let snapshot = entry.snapshot();
        drop(outputs);
        snapshot
    }
    fn write(entry: &Entry, sequence: u64, samples: Vec<i16>) -> Result<(), String> {
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let digest = blake3::hash(&bytes);
        let mut pcm = entry
            .pcm
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        if sequence == pcm.sequence && pcm.last_digest == Some(digest) {
            return Ok(());
        }
        if Some(sequence) != pcm.sequence.checked_add(1) {
            return Err("Stale or reordered audio output packet".into());
        }
        if pcm.queue.len().saturating_add(samples.len()) > MAX_QUEUED_SAMPLES {
            return Ok(());
        }
        pcm.queue.extend(samples);
        pcm.sequence = sequence;
        pcm.last_digest = Some(digest);
        drop(pcm);
        Ok(())
    }
}
struct PreparationGuard {
    shared: Weak<Shared>,
    key: Key,
    generation: u64,
    armed: bool,
}
impl Drop for PreparationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(shared) = self.shared.upgrade()
            && let Ok(mut outputs) = shared.outputs.lock()
            && outputs
                .entries
                .get(&self.key)
                .is_some_and(|entry| entry.generation == self.generation)
        {
            outputs.entries.remove(&self.key);
        }
    }
}
impl AudioOutputServices for AudioOutputService {
    fn output(&self, context: NativeMediaRequestContext, command: Command) -> AudioOutputFuture {
        if !command.valid() {
            return Box::pin(async { Err("Invalid audio output command".into()) });
        }
        let owner = self.clone();
        Box::pin(async move {
            match command {
                Command::Open {} => owner.open(context.into()).await,
                command => owner.control(context.into(), command),
            }
        })
    }
    fn disconnect(&self, peer_id: u64) {
        self.0.stop(Some(peer_id));
    }
}

#[cfg(test)]
mod tests {
    use super::super::collab_broker_capnp::LocalSessionAuthority;
    use super::*;
    use media::pw_stream::DeviceTiming;

    struct Device;
    impl PlaybackStream for Device {
        fn timing(&self) -> Option<DeviceTiming> {
            Some(DeviceTiming::default())
        }
        fn failure(&self) -> Option<String> {
            None
        }
    }
    fn caller(peer: u64, app: &str) -> Caller {
        Caller {
            key: Key {
                peer,
                app: app.into(),
                workspace: "room".into(),
                record: "record".into(),
            },
            current: Arc::new(|| Ok(())),
        }
    }
    type Sources = Arc<Mutex<Vec<Box<dyn PlaybackSource>>>>;
    type Fixture = (LocalSessionAuthority, AudioOutputService, Sources);
    fn fixture() -> Fixture {
        let owner = LocalSessionAuthority::new();
        owner.lifecycle().publish_native_state(false, false);
        let sources = Arc::new(Mutex::new(Vec::new()));
        let retained = sources.clone();
        let service = AudioOutputService::with_factory(
            owner.lifecycle(),
            Arc::new(move |_, source| {
                retained.lock().unwrap().push(source);
                Box::pin(async { Ok(Box::new(Device) as Box<dyn PlaybackStream>) })
            }),
        );
        (owner, service, sources)
    }
    fn render(
        sources: &Arc<Mutex<Vec<Box<dyn PlaybackSource>>>>,
        index: usize,
        samples: usize,
    ) -> Vec<f32> {
        let mut out = vec![0.0; samples];
        sources.lock().unwrap()[index].render(0, &mut out);
        out
    }

    #[test]
    fn governed_output_slow_admission_does_not_hold_the_revocation_lock() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;

        let (owner, service, sources) = fixture();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release = Mutex::new(release_rx);
        let calls = AtomicUsize::new(0);
        let mut music = caller(1, "so.tilo.music");
        music.current = Arc::new(move || {
            // Delay the retained entry's admission check, after the command's
            // own initial check. Other callers and the watchdog remain live.
            if std::thread::current().name() == Some("audio-slow-admission")
                && calls.fetch_add(1, Ordering::AcqRel) == 1
            {
                entered_tx.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let generation = runtime
            .block_on(service.open(music.clone()))
            .unwrap()
            .generation;
        let command_service = service.clone();
        let command = std::thread::Builder::new()
            .name("audio-slow-admission".into())
            .spawn(move || {
                command_service.control(
                    music,
                    Command::Write {
                        generation,
                        sequence: 1,
                        samples: vec![16384; 1920],
                    },
                )
            })
            .unwrap();
        tilo_test_env::recv("the retained admission check", &entered_rx);
        let lifecycle = owner.lifecycle();
        let (locked_tx, locked_rx) = mpsc::sync_channel(1);
        let locking = std::thread::spawn(move || {
            lifecycle.set_locked(true);
            locked_tx.send(()).unwrap();
        });
        // The admission check stays held: locking must finish regardless.
        let revoked_without_waiting = locked_rx.recv_timeout(tilo_test_env::hang_bound()).is_ok();
        // Always release and join before asserting, including on the red path.
        release_tx.send(()).unwrap();
        let result = command.join().unwrap();
        locking.join().unwrap();
        assert!(
            revoked_without_waiting,
            "a blocked admission check delayed the production lock-screen fence"
        );
        assert!(
            result.is_err(),
            "PCM was accepted after the session locked during revalidation"
        );
        assert!(service.0.outputs.lock().unwrap().entries.is_empty());
        assert_eq!(render(&sources, 0, 1920), vec![0.0; 1920]);
    }

    #[test]
    fn governed_output_replacement_survives_an_old_slow_admission_check() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;

        let (_owner, service, sources) = fixture();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release = Mutex::new(release_rx);
        let calls = AtomicUsize::new(0);
        let mut music = caller(1, "so.tilo.music");
        music.current = Arc::new(move || {
            if std::thread::current().name() == Some("audio-slow-admission")
                && calls.fetch_add(1, Ordering::AcqRel) == 1
            {
                entered_tx.send(()).unwrap();
                release.lock().unwrap().recv().unwrap();
                return Err("old admission retired".into());
            }
            Ok(())
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let generation = runtime
            .block_on(service.open(music.clone()))
            .unwrap()
            .generation;
        let command_service = service.clone();
        let command = std::thread::Builder::new()
            .name("audio-slow-admission".into())
            .spawn(move || {
                command_service.control(
                    music,
                    Command::Write {
                        generation,
                        sequence: 1,
                        samples: vec![16384; 1920],
                    },
                )
            })
            .unwrap();
        tilo_test_env::recv("the retained admission check", &entered_rx);
        let replacing_service = service.clone();
        let (replaced_tx, replaced_rx) = mpsc::sync_channel(1);
        let replacing = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(replacing_service.open(caller(1, "so.tilo.music")));
            replaced_tx.send(()).unwrap();
            result
        });
        // The old admission check stays held: the replacement must open regardless.
        let replaced_without_waiting = replaced_rx
            .recv_timeout(tilo_test_env::hang_bound())
            .is_ok();
        release_tx.send(()).unwrap();
        let old_result = command.join().unwrap();
        let replacement = replacing.join().unwrap().unwrap();
        assert!(
            replaced_without_waiting,
            "a stalled old admission check blocked its replacement"
        );
        assert!(old_result.is_err());
        assert_ne!(replacement.generation, generation);
        assert_eq!(render(&sources, 0, 1920), vec![0.0; 1920]);
        service
            .control(
                caller(1, "so.tilo.music"),
                Command::Write {
                    generation: replacement.generation,
                    sequence: 1,
                    samples: vec![16384; 1920],
                },
            )
            .unwrap();
        assert_eq!(render(&sources, 1, 1920), vec![0.5; 1920]);
    }

    #[tokio::test]
    async fn governed_output_lock_fences_preparation_and_revalidation_fences_stream() {
        let owner = LocalSessionAuthority::new();
        owner.lifecycle().publish_native_state(false, false);
        let sources = Arc::new(Mutex::new(Vec::new()));
        let retained = sources.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let preparation = release.clone();
        let service = AudioOutputService::with_factory(
            owner.lifecycle(),
            Arc::new(move |_, source| {
                retained.lock().unwrap().push(source);
                let release = preparation.clone();
                Box::pin(async move {
                    release.notified().await;
                    Ok(Box::new(Device) as Box<dyn PlaybackStream>)
                })
            }),
        );
        let music = caller(1, "so.tilo.music");
        let opening = service.open(music.clone());
        tokio::pin!(opening);
        tokio::select! {
            result = &mut opening => panic!("preparation unexpectedly finished: {result:?}"),
            () = tokio::task::yield_now() => {},
        }
        assert_eq!(sources.lock().unwrap().len(), 1);
        owner.lifecycle().set_locked(true);
        release.notify_one();
        assert!(opening.await.is_err());
        assert!(service.0.outputs.lock().unwrap().entries.is_empty());
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);

        let (_owner, service, sources) = fixture();
        let current = Arc::new(AtomicBool::new(true));
        let checked = current.clone();
        let mut music = music;
        music.current = Arc::new(move || {
            checked
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "process custody ended".into())
        });
        let generation = service.open(music.clone()).await.unwrap().generation;
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        current.store(false, Ordering::Release);
        service.0.revalidate();
        assert!(service.0.outputs.lock().unwrap().entries.is_empty());
        assert!(
            service
                .control(music, Command::Status { generation })
                .is_err()
        );
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
    }

    fn parked(service: &AudioOutputService) -> bool {
        service.0.watch.state.lock().unwrap().parked
    }

    #[tokio::test]
    async fn governed_output_watchdog_parks_without_outputs_and_still_fences_revocation() {
        let (_owner, service, sources) = fixture();
        // No output open: the production watchdog blocks without a timeout.
        tilo_test_env::wait_until("the idle audio authority watchdog to park", || {
            parked(&service)
        });

        let current = Arc::new(AtomicBool::new(true));
        let checked = current.clone();
        let mut music = caller(1, "so.tilo.music");
        music.current = Arc::new(move || {
            checked
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "process custody ended".into())
        });
        let generation = service.open(music.clone()).await.unwrap().generation;
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        // Opening woke it: the lapse is fenced by the watchdog alone, with no
        // command and no manual revalidation, within a few intervals.
        current.store(false, Ordering::Release);
        tilo_test_env::wait_until(
            "the watchdog to fence the output whose admission lapsed",
            || service.0.outputs.lock().unwrap().entries.is_empty(),
        );
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
        assert!(
            service
                .control(music, Command::Status { generation })
                .is_err()
        );
        // Nothing left to watch: it parks again.
        tilo_test_env::wait_until("the watchdog to park after the last output ended", || {
            parked(&service)
        });

        // It exits with the service rather than staying parked forever.
        let watch = Arc::downgrade(&service.0.watch);
        drop(service);
        tilo_test_env::wait_until("the watchdog to exit with its service", || {
            watch.upgrade().is_none()
        });
    }

    #[test]
    fn governed_output_lifecycle_callbacks_work_without_tokio_context() {
        let (owner, service, sources) = fixture();
        let music = caller(1, "so.tilo.music");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let generation = runtime
            .block_on(service.open(music.clone()))
            .unwrap()
            .generation;
        drop(runtime);
        service
            .control(
                music,
                Command::Write {
                    generation,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        owner.lifecycle().set_locked(true);
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
    }

    #[tokio::test]
    async fn governed_output_routes_pcm_and_controls_to_only_the_owning_application() {
        let (_owner, service, sources) = fixture();
        let music = caller(1, "so.tilo.music");
        let other = caller(2, "so.tilo.chat");
        let generation = service.open(music.clone()).await.unwrap().generation;
        let _other_generation = service.open(other.clone()).await.unwrap().generation;
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 1,
                    samples: vec![16384, -16384],
                },
            )
            .unwrap();
        assert_eq!(render(&sources, 0, 2), vec![0.5, -0.5]);
        assert_eq!(render(&sources, 1, 2), vec![0.0, 0.0]);
        assert!(
            service
                .control(other.clone(), Command::Status { generation })
                .is_err()
        );
        service
            .control(
                music.clone(),
                Command::Pause {
                    generation,
                    paused: true,
                },
            )
            .unwrap();
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 2,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
        assert_eq!(
            service
                .control(music.clone(), Command::Status { generation })
                .unwrap()
                .queued_frames,
            1
        );
        service
            .control(
                music.clone(),
                Command::Volume {
                    generation,
                    percent: 50,
                },
            )
            .unwrap();
        service
            .control(
                music.clone(),
                Command::Pause {
                    generation,
                    paused: false,
                },
            )
            .unwrap();
        assert_eq!(render(&sources, 0, 2), vec![0.25; 2]);
        let status = service
            .control(music.clone(), Command::Status { generation })
            .unwrap();
        assert_eq!(status.rendered_frames, 2);
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 3,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        service
            .control(music.clone(), Command::Flush { generation })
            .unwrap();
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
    }

    #[tokio::test]
    async fn governed_output_peer_disconnect_fences_only_its_pcm() {
        let (_owner, service, sources) = fixture();
        let music = caller(1, "so.tilo.music");
        let other = caller(2, "so.tilo.chat");
        let generation = service.open(music.clone()).await.unwrap().generation;
        let other_generation = service.open(other.clone()).await.unwrap().generation;
        service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        service.disconnect(1);
        assert!(
            service
                .control(music, Command::Status { generation })
                .is_err()
        );
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
        assert!(
            !service
                .control(
                    other,
                    Command::Status {
                        generation: other_generation
                    }
                )
                .unwrap()
                .closed
        );
    }

    #[tokio::test]
    async fn governed_output_replacement_and_lock_fence_pcm() {
        let (owner, service, sources) = fixture();
        let music = caller(1, "so.tilo.music");
        let first = service.open(music.clone()).await.unwrap().generation;
        service
            .control(
                music.clone(),
                Command::Write {
                    generation: first,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        let next = service.open(music.clone()).await.unwrap().generation;
        assert!(next > first);
        assert_eq!(render(&sources, 0, 2), vec![0.0; 2]);
        assert!(
            service
                .control(music.clone(), Command::Close { generation: first })
                .is_err()
        );
        service
            .control(
                music.clone(),
                Command::Write {
                    generation: next,
                    sequence: 1,
                    samples: vec![16384; 2],
                },
            )
            .unwrap();
        owner.lifecycle().set_locked(true);
        assert_eq!(render(&sources, 1, 2), vec![0.0; 2]);
        assert!(service.open(music.clone()).await.is_err());
        owner.lifecycle().set_locked(false);
        let third = service.open(music.clone()).await.unwrap().generation;
        assert!(third > next);
        assert!(
            service
                .control(music, Command::Status { generation: next })
                .is_err()
        );
    }

    #[tokio::test]
    async fn governed_output_bounds_backpressure_and_rejects_changed_or_reordered_retries() {
        let (_owner, service, _sources) = fixture();
        let music = caller(1, "so.tilo.music");
        let generation = service.open(music.clone()).await.unwrap().generation;
        for sequence in 1..=8 {
            service
                .control(
                    music.clone(),
                    Command::Write {
                        generation,
                        sequence,
                        samples: vec![1; MAX_SAMPLES],
                    },
                )
                .unwrap();
        }
        let full = service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 9,
                    samples: vec![1; MAX_SAMPLES],
                },
            )
            .unwrap();
        assert_eq!(full.accepted_sequence, 8);
        assert_eq!(full.queued_frames, 7680);
        assert!(
            service
                .control(
                    music.clone(),
                    Command::Write {
                        generation,
                        sequence: 8,
                        samples: vec![2; MAX_SAMPLES]
                    }
                )
                .is_err()
        );
        let duplicate = service
            .control(
                music.clone(),
                Command::Write {
                    generation,
                    sequence: 8,
                    samples: vec![1; MAX_SAMPLES],
                },
            )
            .unwrap();
        assert_eq!(duplicate.queued_frames, full.queued_frames);
        service
            .control(music.clone(), Command::Flush { generation })
            .unwrap();
        let accepted = service
            .control(
                music,
                Command::Write {
                    generation,
                    sequence: 9,
                    samples: vec![1; MAX_SAMPLES],
                },
            )
            .unwrap();
        assert_eq!(accepted.accepted_sequence, 9);
        assert_eq!(accepted.queued_frames, 960);
    }
}
