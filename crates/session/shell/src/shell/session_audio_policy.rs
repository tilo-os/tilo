//! One session-lifetime owner for persistent audio transforms.
//!
//! Settings writes intent to the shared settings store. This worker reads it,
//! observes `PipeWire` off the render thread, and owns any virtual mono node
//! and the echo-cancelled microphone until the desktop session ends. It is
//! intentionally not a `DBus` service: neither operation is privileged, and
//! one owner per graphical session is the security/correctness boundary we
//! need. The same observation reports audio devices arriving and leaving, for
//! the "device connected/disconnected" system sounds.
//!
//! It also watches for hotplug: when the output sound was going to
//! was a headset, headphones or a Bluetooth sink and it disappears, playing
//! media is paused rather than suddenly coming out of the speakers — as on a
//! phone. Plugging the device back in never resumes it.
//!
//! Observation is event-driven. A native registry watch
//! ([`media::audio_watch`]) reports device changes within
//! [`media::audio_watch::DEBOUNCE`], the settings file watch reports
//! preference changes, and between them the worker blocks with no timer: an
//! idle desktop neither shells `pw-dump` nor re-reads settings. Only when
//! `PipeWire` cannot be watched does it fall back to polling — slower, but
//! still observing, never blind.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use media::audio::{AudioDevice, AudioGraph, GraphDelta};
use media::audio_runtime::{PersistentAudioPolicy, PersistentAudioPreferences, RealAudioRuntime};
use media::audio_watch::{AudioGraphWatch, GraphWatchEvent};
use settings_store::system_sounds::SystemSoundEvent;

/// Fallback only, while `PipeWire` cannot be watched: how often to shell
/// `pw-dump` while the graph is still settling.
const POLL_INTERVAL: Duration = Duration::from_millis(1200);
/// Fallback only: once the graph stays still, poll slower — but keep
/// enumerating, so hotplug is still seen within this window.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(8);
const STABLE_POLLS_BEFORE_IDLE: u8 = 3;

fn poll_interval(stable_polls: u8) -> Duration {
    if stable_polls >= STABLE_POLLS_BEFORE_IDLE {
        IDLE_POLL_INTERVAL
    } else {
        POLL_INTERVAL
    }
}

/// Devices and hardware only. Streams (including the `pw-dump` this
/// worker shells) are not part of "the graph is still": comparing
/// [`AudioGraph::topology_signature`] kept the enumerator awake because
/// each dump is itself a playback stream.
fn idle_signature(graph: &AudioGraph) -> String {
    let names = |devices: &[AudioDevice]| {
        devices
            .iter()
            .map(|device| device.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "sink[{}]|src[{}]|hw[{}]",
        names(&graph.sinks),
        names(&graph.sources),
        graph
            .hardware
            .iter()
            .map(|device| format!("{}:{}", device.id, device.profiles.len()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn topology_unchanged(previous: Option<&str>, graph: &AudioGraph) -> bool {
    previous == Some(idle_signature(graph).as_str())
}

/// What wakes the policy worker. While `PipeWire` can be watched nothing
/// else does: there is no timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PolicyEvent {
    /// The device graph changed, or the watch just started (the baseline).
    GraphChanged,
    /// The watch lost `PipeWire`: poll until it can watch again.
    WatchLost,
    /// The settings file changed.
    SettingsChanged,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Event(PolicyEvent),
    /// A fallback poll or a policy retry fell due.
    Timeout,
}

/// The worker's side effects, so the production loop is tested without
/// `PipeWire`, `MPRIS` or a settings file.
trait PolicyEnv {
    /// Read the whole graph.
    fn enumerate(&mut self) -> AudioGraph;
    fn load_preferences(&mut self) -> PersistentAudioPreferences;
    /// Pause every playing player; `false` when that failed.
    fn pause_playing_players(&mut self) -> bool;
    /// Apply `preferences` to `graph`.
    fn reconcile(
        &mut self,
        preferences: PersistentAudioPreferences,
        graph: &AudioGraph,
    ) -> Result<(), String>;
    /// When the policy must reconcile again although nothing changed.
    fn next_retry(&self) -> Option<Instant>;
    fn device_sound(&mut self, event: SystemSoundEvent);
    /// (Re)start the native graph watch; `false` when `PipeWire` cannot be
    /// watched.
    fn watch_graph(&mut self) -> bool;
    /// Start the settings file watch; `false` when it cannot run.
    fn watch_settings(&mut self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    /// The native watch reports every change; sleep until one arrives.
    Watching,
    /// `PipeWire` could not be watched: poll `pw-dump` instead, enumerating
    /// on every poll.
    Polling { stable_polls: u8 },
}

struct PolicyWorker {
    observation: Observation,
    preferences: PersistentAudioPreferences,
    /// Without a settings watch, preferences are re-read with each graph
    /// change instead — still no periodic parse.
    reload_preferences_with_graph: bool,
    devices: DeviceRoster,
    last_graph: Option<AudioGraph>,
    last_error: Option<String>,
}

impl PolicyWorker {
    fn start(env: &mut impl PolicyEnv) -> Self {
        let reload_preferences_with_graph = !env.watch_settings();
        let mut worker = Self {
            observation: Observation::Watching,
            preferences: env.load_preferences(),
            reload_preferences_with_graph,
            devices: DeviceRoster::default(),
            last_graph: None,
            last_error: None,
        };
        // A running watch sends the baseline itself.
        if !env.watch_graph() {
            tracing::warn!(
                target: "tilo::audio_policy",
                "PipeWire cannot be watched; polling the audio graph instead"
            );
            worker.observation = Observation::Polling { stable_polls: 0 };
            worker.observe(env);
        }
        worker
    }

    /// How long to wait for an event before acting anyway. `None` — block
    /// until an event — whenever the graph is watched and no retry is due.
    fn next_wake(&self, now: Instant, retry: Option<Instant>) -> Option<Duration> {
        let poll = match self.observation {
            Observation::Watching => None,
            Observation::Polling { stable_polls } => Some(poll_interval(stable_polls)),
        };
        // A retry already past was attempted; the next change retries again.
        let retry = retry
            .filter(|at| *at > now)
            .map(|at| at.saturating_duration_since(now));
        match (poll, retry) {
            (Some(poll), Some(retry)) => Some(poll.min(retry)),
            (poll, retry) => poll.or(retry),
        }
    }

    fn handle(&mut self, wake: Wake, env: &mut impl PolicyEnv) {
        match wake {
            Wake::Event(PolicyEvent::GraphChanged) => self.observe(env),
            Wake::Event(PolicyEvent::WatchLost) => {
                if self.observation == Observation::Watching {
                    tracing::warn!(
                        target: "tilo::audio_policy",
                        "lost the PipeWire watch; polling the audio graph until it is back"
                    );
                    self.observation = Observation::Polling { stable_polls: 0 };
                    self.observe(env);
                }
            }
            Wake::Event(PolicyEvent::SettingsChanged) => {
                let preferences = env.load_preferences();
                if preferences != self.preferences {
                    self.preferences = preferences;
                    self.reconcile(env);
                }
            }
            Wake::Event(PolicyEvent::Stop) => {}
            Wake::Timeout => match self.observation {
                Observation::Polling { .. } if env.watch_graph() => {
                    tracing::info!(
                        target: "tilo::audio_policy",
                        "watching the PipeWire audio graph again"
                    );
                    self.observation = Observation::Watching;
                }
                Observation::Polling { .. } => self.observe(env),
                Observation::Watching => self.reconcile(env),
            },
        }
    }

    /// Read the graph and act on what changed since the last reading.
    fn observe(&mut self, env: &mut impl PolicyEnv) {
        if self.reload_preferences_with_graph {
            self.preferences = env.load_preferences();
        }
        let graph = env.enumerate();
        if let Observation::Polling { stable_polls } = &mut self.observation {
            let previous = self.last_graph.as_ref().map(idle_signature);
            *stable_polls = if topology_unchanged(previous.as_deref(), &graph) {
                stable_polls.saturating_add(1)
            } else {
                0
            };
        }
        for event in self.devices.observe(&graph) {
            env.device_sound(event);
        }
        if let Some(previous) = self.last_graph.as_ref()
            && enumerated(&graph)
        {
            let delta = AudioGraph::diff(previous, &graph);
            if pause_on_removal(previous, &delta) && !env.pause_playing_players() {
                tracing::warn!(
                    target: "tilo::audio_policy",
                    "private output removed but players could not be paused"
                );
            }
        }
        self.last_graph = Some(graph);
        self.reconcile(env);
    }

    fn reconcile(&mut self, env: &mut impl PolicyEnv) {
        let Some(graph) = self.last_graph.as_ref() else {
            // The baseline reconciles once it arrives.
            return;
        };
        let error = env.reconcile(self.preferences, graph).err();
        if error != self.last_error {
            match error.as_deref() {
                Some(error) => tracing::warn!(
                    target: "tilo::audio_policy",
                    %error,
                    "persistent audio preference could not be applied"
                ),
                None if self.last_error.is_some() => tracing::info!(
                    target: "tilo::audio_policy",
                    "persistent audio preference recovered"
                ),
                None => {}
            }
            self.last_error = error;
        }
    }
}

/// The worker's loop: block for the next event, or until the fallback poll
/// or a policy retry falls due.
fn run(env: &mut impl PolicyEnv, events: &Receiver<PolicyEvent>) {
    let mut worker = PolicyWorker::start(env);
    loop {
        let wake = match worker.next_wake(Instant::now(), env.next_retry()) {
            None => events
                .recv()
                .map_or(Wake::Event(PolicyEvent::Stop), Wake::Event),
            Some(after) => match events.recv_timeout(after) {
                Ok(event) => Wake::Event(event),
                Err(RecvTimeoutError::Timeout) => Wake::Timeout,
                Err(RecvTimeoutError::Disconnected) => Wake::Event(PolicyEvent::Stop),
            },
        };
        if wake == Wake::Event(PolicyEvent::Stop) {
            break;
        }
        worker.handle(wake, env);
    }
}

/// The real session: `pw-dump`, `MPRIS`, the settings store and the native
/// watches. Field order is drop order: the watches stop before the policy's
/// bounded mono session restores the physical default.
struct LiveEnv {
    graph_watch: Option<AudioGraphWatch>,
    settings_watch: Option<settings_store::watch::SettingsWatch>,
    events: Sender<PolicyEvent>,
    device_sounds: Sender<SystemSoundEvent>,
    policy: PersistentAudioPolicy,
}

impl PolicyEnv for LiveEnv {
    fn enumerate(&mut self) -> AudioGraph {
        media::audio::enumerate()
    }

    fn load_preferences(&mut self) -> PersistentAudioPreferences {
        load_preferences()
    }

    fn pause_playing_players(&mut self) -> bool {
        crate::shell::mpris::pause_playing_players()
    }

    fn reconcile(
        &mut self,
        preferences: PersistentAudioPreferences,
        graph: &AudioGraph,
    ) -> Result<(), String> {
        self.policy.reconcile(preferences, graph)
    }

    fn next_retry(&self) -> Option<Instant> {
        self.policy.next_retry()
    }

    fn device_sound(&mut self, event: SystemSoundEvent) {
        let _ = self.device_sounds.send(event);
    }

    fn watch_graph(&mut self) -> bool {
        // One watch at a time: join a lost one before connecting again.
        self.graph_watch = None;
        let events = self.events.clone();
        self.graph_watch = media::audio_watch::watch(move |event| {
            let _ = events.send(match event {
                GraphWatchEvent::Changed => PolicyEvent::GraphChanged,
                GraphWatchEvent::Lost => PolicyEvent::WatchLost,
            });
        })
        .inspect_err(
            |error| tracing::debug!(target: "tilo::audio_policy", %error, "audio graph watch"),
        )
        .ok();
        self.graph_watch.is_some()
    }

    fn watch_settings(&mut self) -> bool {
        let events = self.events.clone();
        self.settings_watch = settings_store::watch::watch_settings(move || {
            let _ = events.send(PolicyEvent::SettingsChanged);
        })
        .inspect_err(|error| {
            tracing::warn!(
                target: "tilo::audio_policy",
                %error,
                "settings cannot be watched; re-reading them with each audio graph change"
            );
        })
        .ok();
        self.settings_watch.is_some()
    }
}

#[derive(Debug)]
pub struct SessionAudioPolicyOwner {
    events: Sender<PolicyEvent>,
    worker: Option<JoinHandle<()>>,
    device_sounds: Mutex<Receiver<SystemSoundEvent>>,
}

impl SessionAudioPolicyOwner {
    /// Spawns the worker thread.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the thread could not be spawned (resource
    /// exhaustion) — the caller runs the session without persistent audio
    /// transforms rather than tearing the whole desktop down over it.
    pub fn spawn() -> std::io::Result<Self> {
        let (events, rx) = mpsc::channel();
        let (device_tx, device_sounds) = mpsc::channel();
        let worker_events = events.clone();
        let worker = std::thread::Builder::new()
            .name("tilo-session-audio-policy".into())
            .spawn(move || {
                let mut env = LiveEnv {
                    graph_watch: None,
                    settings_watch: None,
                    events: worker_events,
                    device_sounds: device_tx,
                    policy: PersistentAudioPolicy::new(Arc::new(RealAudioRuntime)),
                };
                run(&mut env, &rx);
            })?;
        Ok(Self {
            events,
            worker: Some(worker),
            device_sounds: Mutex::new(device_sounds),
        })
    }

    /// Audio devices that arrived or left since the last call, as the
    /// system sounds they stand for.
    pub fn drain_device_sounds(&self) -> Vec<SystemSoundEvent> {
        self.device_sounds
            .lock()
            .map(|events| events.try_iter().collect())
            .unwrap_or_default()
    }
}

/// The audio hardware present at the last observation, by description.
///
/// Devices, not nodes: a Bluetooth headset switching between its music and
/// call profiles replaces its nodes but stays one device. The first
/// observation is the baseline, and a graph with no hardware at all is
/// treated as "the audio service could not be read" rather than every device
/// leaving at once.
#[derive(Debug, Default)]
struct DeviceRoster {
    known: Option<Vec<String>>,
}

impl DeviceRoster {
    fn observe(&mut self, graph: &media::audio::AudioGraph) -> Vec<SystemSoundEvent> {
        if graph.hardware.is_empty() {
            return Vec::new();
        }
        let mut now: Vec<String> = graph
            .hardware
            .iter()
            .map(|device| device.description.clone())
            .collect();
        now.sort();
        let Some(before) = self.known.replace(now.clone()) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        if multiset_difference(&now, &before) {
            events.push(SystemSoundEvent::DeviceConnected);
        }
        if multiset_difference(&before, &now) {
            events.push(SystemSoundEvent::DeviceDisconnected);
        }
        events
    }
}

/// Whether sorted `a` holds an item (counting repeats) that sorted `b` lacks.
fn multiset_difference(a: &[String], b: &[String]) -> bool {
    let mut rest = b.iter().peekable();
    for item in a {
        while rest.peek().is_some_and(|other| *other < item) {
            rest.next();
        }
        if rest.peek() == Some(&item) {
            rest.next();
        } else {
            return true;
        }
    }
    false
}

impl Drop for SessionAudioPolicyOwner {
    fn drop(&mut self) {
        let _ = self.events.send(PolicyEvent::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Whether an enumeration actually saw the graph. `pw-dump` failing reads as
/// an empty graph, which must not look like every device being unplugged.
fn enumerated(graph: &AudioGraph) -> bool {
    !graph.sinks.is_empty() || !graph.sources.is_empty()
}

/// Sound on this output reaches only the person wearing or holding it: a
/// Bluetooth sink, a headset or headphones. Losing it while playing means
/// the next output is likely a room's speakers.
pub(crate) fn private_output(device: &AudioDevice) -> bool {
    let name = device.name.to_ascii_lowercase();
    let description = device.description.to_ascii_lowercase();
    device.codec.is_some()
        || name.starts_with("bluez_output.")
        || name.starts_with("bluez_sink.")
        || ["headset", "headphone", "earphone", "earbud", "airpods"]
            .iter()
            .any(|word| name.contains(word) || description.contains(word))
}

/// The hotplug decision: pause playing media when the output that was the
/// default is gone and it was a private one. A removed speaker, a removed
/// non-default device and a device appearing never pause — and nothing here
/// ever resumes.
pub(crate) fn pause_on_removal(previous: &AudioGraph, delta: &GraphDelta) -> bool {
    let Some(default) = previous.default_sink() else {
        return false;
    };
    delta
        .removed_sinks
        .iter()
        .any(|removed| removed.name == default.name)
        && private_output(default)
}

fn load_preferences() -> PersistentAudioPreferences {
    let doc = settings_store::load();
    preferences_from(&doc)
}

fn preferences_from(
    doc: &serde_json::Map<String, serde_json::Value>,
) -> PersistentAudioPreferences {
    // Clamped to 0.0..=1.0 above; f32's precision loss over that tiny
    // range is imperceptible for a pan-balance control.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "value is clamped to 0.0..=1.0 by f64_in; narrowing loses no meaningful precision"
    )]
    let balance = settings_store::f64_in(doc, "audio", "balance", 0.0..=1.0, 0.5) as f32;
    PersistentAudioPreferences {
        balance,
        mono_output: settings_store::bool_at(doc, "audio", "mono_output").unwrap_or(false),
        echo_cancellation: settings_store::bool_at(doc, "audio", "echo_cancellation")
            .unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sink(name: &str, description: &str, is_default: bool) -> AudioDevice {
        AudioDevice {
            id: 1,
            serial: 1,
            name: name.into(),
            description: description.into(),
            is_default,
            volume: None,
            muted: None,
            channels: 2,
            codec: None,
        }
    }

    fn sinks_graph(sinks: Vec<AudioDevice>) -> AudioGraph {
        AudioGraph {
            sinks,
            ..AudioGraph::default()
        }
    }

    #[test]
    fn unplugging_the_default_private_output_pauses_and_nothing_else_does() {
        let speakers = || {
            sink(
                "alsa_output.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio Analog Stereo",
                false,
            )
        };
        let bluetooth = |default| {
            let mut device = sink("bluez_output.AA_BB.1", "Buds", default);
            device.codec = Some("AAC".into());
            device
        };
        let headset = |default| {
            sink(
                "alsa_output.usb-Logi_USB_Headset-00.analog-stereo",
                "USB Headset",
                default,
            )
        };
        let as_default = |mut device: AudioDevice| {
            device.is_default = true;
            device
        };
        // (before, after, pauses)
        let table = [
            // Headphones / Bluetooth removed while default: pause.
            (vec![speakers(), bluetooth(true)], vec![speakers()], true),
            (vec![speakers(), headset(true)], vec![speakers()], true),
            // Default speakers removed: keep playing.
            (
                vec![as_default(speakers()), headset(false)],
                vec![headset(false)],
                false,
            ),
            // A private output removed while it was not the default.
            (
                vec![as_default(speakers()), bluetooth(false)],
                vec![as_default(speakers())],
                false,
            ),
            // Plugging headphones in, or back in, never resumes or pauses.
            (
                vec![as_default(speakers())],
                vec![speakers(), headset(true)],
                false,
            ),
            // Nothing changed.
            (
                vec![speakers(), headset(true)],
                vec![speakers(), headset(true)],
                false,
            ),
        ];
        for (index, (before, after, pauses)) in table.into_iter().enumerate() {
            let before = sinks_graph(before);
            let after = sinks_graph(after);
            let delta = AudioGraph::diff(&before, &after);
            assert_eq!(pause_on_removal(&before, &delta), pauses, "row {index}");
        }
    }

    #[test]
    fn a_failed_enumeration_is_not_an_unplug() {
        assert!(!enumerated(&AudioGraph::default()));
        assert!(enumerated(&sinks_graph(vec![sink("a", "A", true)])));
    }

    #[test]
    fn a_stable_graph_backs_off_pw_dump() {
        assert_eq!(poll_interval(0), POLL_INTERVAL);
        assert_eq!(poll_interval(STABLE_POLLS_BEFORE_IDLE - 1), POLL_INTERVAL);
        assert_eq!(poll_interval(STABLE_POLLS_BEFORE_IDLE), IDLE_POLL_INTERVAL);
        assert_eq!(poll_interval(u8::MAX), IDLE_POLL_INTERVAL);
        assert!(IDLE_POLL_INTERVAL > POLL_INTERVAL);
    }

    #[test]
    fn empty_and_volume_only_graphs_are_topology_stable() {
        let empty = AudioGraph::default();
        assert!(
            topology_unchanged(Some(idle_signature(&empty).as_str()), &empty),
            "two empty dumps are the same topology; PartialEq used to miss this              when last_graph stayed unset"
        );
        let mut louder = graph(&["Built-in Audio"]);
        louder.sinks.push(sink(
            "alsa_output.pci.analog-stereo",
            "Built-in Audio",
            true,
        ));
        let mut quieter = louder.clone();
        if let Some(device) = quieter.sinks.first_mut() {
            device.volume = Some(0.1);
        }
        assert_eq!(
            louder.topology_signature(),
            quieter.topology_signature(),
            "volume is not topology; comparing whole graphs kept pw-dump awake"
        );
    }

    #[test]
    fn a_transient_dump_stream_does_not_keep_the_graph_awake() {
        let mut still = graph(&["Built-in Audio"]);
        still.sinks.push(sink(
            "alsa_output.pci.analog-stereo",
            "Built-in Audio",
            true,
        ));
        let mut with_dump = still.clone();
        with_dump.playback_streams.push(media::audio::AudioStream {
            id: 99,
            serial: 1,
            app: "pw-dump".into(),
            media_name: None,
            playback: true,
            target_id: None,
            volume: None,
            muted: None,
        });
        assert_ne!(
            still.topology_signature(),
            with_dump.topology_signature(),
            "topology_signature still sees streams; idle compare must not"
        );
        assert!(
            topology_unchanged(Some(idle_signature(&still).as_str()), &with_dump),
            "pw-dump is a stream, not a device; including it in idle compare kept itself awake"
        );
    }

    /// The worker's world without `PipeWire`: each enumeration returns the
    /// next scripted graph (then keeps returning the last one), and every
    /// side effect is counted.
    struct FakeEnv {
        graphs: std::collections::VecDeque<AudioGraph>,
        last: AudioGraph,
        preferences: PersistentAudioPreferences,
        graph_watchable: bool,
        settings_watchable: bool,
        enumerations: usize,
        preference_loads: usize,
        pauses: usize,
        reconciles: usize,
        sounds: Vec<SystemSoundEvent>,
    }

    impl FakeEnv {
        fn new(graphs: impl IntoIterator<Item = AudioGraph>) -> Self {
            Self {
                graphs: graphs.into_iter().collect(),
                last: AudioGraph::default(),
                preferences: PersistentAudioPreferences::default(),
                graph_watchable: true,
                settings_watchable: true,
                enumerations: 0,
                preference_loads: 0,
                pauses: 0,
                reconciles: 0,
                sounds: Vec::new(),
            }
        }
    }

    impl PolicyEnv for FakeEnv {
        fn enumerate(&mut self) -> AudioGraph {
            self.enumerations = self.enumerations.saturating_add(1);
            if let Some(graph) = self.graphs.pop_front() {
                self.last = graph;
            }
            self.last.clone()
        }
        fn load_preferences(&mut self) -> PersistentAudioPreferences {
            self.preference_loads = self.preference_loads.saturating_add(1);
            self.preferences
        }
        fn pause_playing_players(&mut self) -> bool {
            self.pauses = self.pauses.saturating_add(1);
            true
        }
        fn reconcile(
            &mut self,
            _preferences: PersistentAudioPreferences,
            _graph: &AudioGraph,
        ) -> Result<(), String> {
            self.reconciles = self.reconciles.saturating_add(1);
            Ok(())
        }
        fn next_retry(&self) -> Option<Instant> {
            None
        }
        fn device_sound(&mut self, event: SystemSoundEvent) {
            self.sounds.push(event);
        }
        fn watch_graph(&mut self) -> bool {
            self.graph_watchable
        }
        fn watch_settings(&mut self) -> bool {
            self.settings_watchable
        }
    }

    /// A laptop's speakers, and a USB headset while it is plugged in — the
    /// default output whenever it is there.
    fn laptop(headset: bool) -> AudioGraph {
        let mut sinks = vec![sink(
            "alsa_output.pci-0000_00_1f.3.analog-stereo",
            "Built-in Audio Analog Stereo",
            !headset,
        )];
        let mut hardware = vec!["Built-in Audio"];
        if headset {
            sinks.push(sink(
                "alsa_output.usb-Logi_USB_Headset-00.analog-stereo",
                "USB Headset",
                true,
            ));
            hardware.push("USB Headset");
        }
        AudioGraph {
            sinks,
            ..graph(&hardware)
        }
    }

    /// Far more still observations than the three after which `bbbeed0a87`
    /// stopped enumerating for good.
    const LONG_STILL: usize = 20;
    /// The baseline and every still reading after it.
    const STILL_READINGS: usize = LONG_STILL + 1;

    #[test]
    fn removal_of_private_output_pauses_after_settling() {
        let mut graphs = vec![laptop(true); LONG_STILL];
        graphs.push(laptop(false));
        let (events, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut env = FakeEnv::new(graphs);
            run(&mut env, &rx);
            env
        });
        for _ in 0..=LONG_STILL {
            events.send(PolicyEvent::GraphChanged).unwrap();
        }
        events.send(PolicyEvent::Stop).unwrap();
        let env = worker.join().unwrap();
        assert_eq!(env.enumerations, STILL_READINGS, "one reading per change");
        assert_eq!(
            env.pauses, 1,
            "unplugging the headset after the graph settled must pause playback"
        );
        assert_eq!(env.sounds, [SystemSoundEvent::DeviceDisconnected]);
    }

    #[test]
    fn idle_session_skips_pw_dump_until_the_graph_changes() {
        let mut env = FakeEnv::new([laptop(true)]);
        let mut worker = PolicyWorker::start(&mut env);
        assert_eq!(
            env.enumerations, 0,
            "the watch's baseline is the first read"
        );
        worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        let now = Instant::now();
        for later in [0, 4, 30, 3600] {
            assert_eq!(
                worker.next_wake(now.checked_add(Duration::from_secs(later)).unwrap(), None),
                None,
                "R01's 30s empty desktop must not shell pw-dump at all once idle"
            );
        }
        assert_eq!(env.enumerations, 1);
        assert_eq!(env.preference_loads, 1, "settings are read on change only");
        for _ in 0..LONG_STILL {
            worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        }
        assert_eq!(
            env.enumerations, STILL_READINGS,
            "a change after any amount of stillness is still read"
        );
        assert_eq!(worker.next_wake(now, None), None);
    }

    #[test]
    fn unreachable_pipewire_falls_back_to_polling_that_still_sees_hotplug() {
        let mut env = FakeEnv::new([laptop(true)]);
        env.graph_watchable = false;
        let mut worker = PolicyWorker::start(&mut env);
        assert_eq!(env.enumerations, 1, "polling reads its own baseline");
        let now = Instant::now();
        assert_eq!(worker.next_wake(now, None), Some(POLL_INTERVAL));
        for _ in 0..LONG_STILL {
            worker.handle(Wake::Timeout, &mut env);
        }
        assert_eq!(
            worker.next_wake(now, None),
            Some(IDLE_POLL_INTERVAL),
            "a still graph is polled slower"
        );
        assert_eq!(
            env.enumerations, STILL_READINGS,
            "but every poll still reads the graph"
        );
        env.graphs.push_back(laptop(false));
        worker.handle(Wake::Timeout, &mut env);
        assert_eq!(env.pauses, 1, "hotplug lands within one idle poll");
        assert_eq!(env.sounds, [SystemSoundEvent::DeviceDisconnected]);

        env.graph_watchable = true;
        let before = env.enumerations;
        worker.handle(Wake::Timeout, &mut env);
        assert_eq!(worker.next_wake(now, None), None, "watching again");
        assert_eq!(env.enumerations, before, "the watch sends the baseline");
    }

    #[test]
    fn a_lost_watch_polls_until_it_is_back() {
        let mut env = FakeEnv::new([laptop(true)]);
        let mut worker = PolicyWorker::start(&mut env);
        worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        worker.handle(Wake::Event(PolicyEvent::WatchLost), &mut env);
        assert_eq!(env.enumerations, 2, "a lost watch reads the graph at once");
        assert_eq!(worker.next_wake(Instant::now(), None), Some(POLL_INTERVAL));
    }

    #[test]
    fn settings_changes_reconcile_without_reading_the_graph() {
        let mut env = FakeEnv::new([laptop(true)]);
        let mut worker = PolicyWorker::start(&mut env);
        worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        let (reads, reconciles) = (env.enumerations, env.reconciles);
        worker.handle(Wake::Event(PolicyEvent::SettingsChanged), &mut env);
        assert_eq!(env.reconciles, reconciles, "an unrelated setting changed");
        env.preferences.mono_output = true;
        worker.handle(Wake::Event(PolicyEvent::SettingsChanged), &mut env);
        assert_eq!(env.reconciles, reconciles.saturating_add(1));
        assert_eq!(env.enumerations, reads, "the graph did not change");
    }

    #[test]
    fn without_a_settings_watch_preferences_follow_graph_changes() {
        let mut env = FakeEnv::new([laptop(true)]);
        env.settings_watchable = false;
        let mut worker = PolicyWorker::start(&mut env);
        worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        assert_eq!(env.preference_loads, 2);
        assert_eq!(worker.next_wake(Instant::now(), None), None);
    }

    #[test]
    fn a_due_retry_wakes_once_and_a_past_one_never_spins() {
        let mut env = FakeEnv::new([laptop(true)]);
        let mut worker = PolicyWorker::start(&mut env);
        worker.handle(Wake::Event(PolicyEvent::GraphChanged), &mut env);
        let now = Instant::now();
        let retry = now.checked_add(Duration::from_secs(5)).unwrap();
        assert_eq!(
            worker.next_wake(now, Some(retry)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(worker.next_wake(retry, Some(retry)), None);
        let reconciles = env.reconciles;
        worker.handle(Wake::Timeout, &mut env);
        assert_eq!(
            env.reconciles,
            reconciles.saturating_add(1),
            "the retry reconciles"
        );
        assert_eq!(env.enumerations, 1, "against the graph it already has");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_idle_worker_thread_stays_asleep() {
        const COMM: &str = "tilo-audio-idle";
        let ((events, worker), tid) = tilo_test_env::idle::spawned_thread(COMM, || {
            let (events, rx) = mpsc::channel();
            let worker = std::thread::Builder::new()
                .name(COMM.into())
                .spawn(move || {
                    let mut env = FakeEnv::new([laptop(true)]);
                    run(&mut env, &rx);
                    env
                })
                .unwrap();
            events.send(PolicyEvent::GraphChanged).unwrap();
            (events, worker)
        });
        tilo_test_env::idle::assert_stays_asleep(&tid, "audio policy worker");
        events.send(PolicyEvent::Stop).unwrap();
        let env = worker.join().unwrap();
        assert_eq!(env.enumerations, 1);
        assert_eq!(env.preference_loads, 1);
    }

    #[test]
    fn private_outputs_are_recognised_by_transport_and_name() {
        assert!(private_output(&sink("bluez_output.X.1", "Speaker", false)));
        assert!(private_output(&sink("alsa_output.x", "Headphones", false)));
        assert!(!private_output(&sink(
            "alsa_output.pci.hdmi-stereo",
            "HDMI / DisplayPort",
            false
        )));
        assert!(!private_output(&sink(
            "alsa_output.pci.analog-stereo",
            "Built-in Audio",
            false
        )));
    }

    #[test]
    fn malformed_or_out_of_range_preferences_are_safe_defaults() {
        let mut doc = serde_json::Map::new();
        doc.insert(
            "audio".into(),
            json!({"balance": 8.0, "mono_output": "yes", "echo_cancellation": "no"}),
        );
        assert_eq!(
            preferences_from(&doc),
            PersistentAudioPreferences::default()
        );
    }

    fn graph(devices: &[&str]) -> media::audio::AudioGraph {
        media::audio::AudioGraph {
            hardware: devices
                .iter()
                .enumerate()
                .map(|(id, name)| media::audio::AudioHardwareDevice {
                    id: u32::try_from(id).unwrap_or(0),
                    description: (*name).into(),
                    active_profile: None,
                    profiles: Vec::new(),
                })
                .collect(),
            ..media::audio::AudioGraph::default()
        }
    }

    #[test]
    fn devices_arriving_and_leaving_become_system_sounds() {
        let mut roster = DeviceRoster::default();
        assert!(
            roster.observe(&graph(&["Built-in Audio"])).is_empty(),
            "baseline"
        );
        assert!(roster.observe(&graph(&["Built-in Audio"])).is_empty());
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio", "Headset"])),
            [SystemSoundEvent::DeviceConnected]
        );
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio", "Headset", "Headset"])),
            [SystemSoundEvent::DeviceConnected],
            "a second identical headset is a new device"
        );
        assert!(
            roster.observe(&graph(&[])).is_empty(),
            "an unreadable graph is not every device leaving"
        );
        assert_eq!(
            roster.observe(&graph(&["Built-in Audio"])),
            [SystemSoundEvent::DeviceDisconnected]
        );
        assert_eq!(
            roster.observe(&graph(&["Dock", "Headset"])),
            [
                SystemSoundEvent::DeviceConnected,
                SystemSoundEvent::DeviceDisconnected
            ]
        );
    }

    #[test]
    fn calls_are_echo_cancelled_unless_the_person_turned_it_off() {
        assert!(preferences_from(&serde_json::Map::new()).echo_cancellation);
        let mut doc = serde_json::Map::new();
        doc.insert("audio".into(), json!({"echo_cancellation": false}));
        assert!(!preferences_from(&doc).echo_cancellation);
    }
}
