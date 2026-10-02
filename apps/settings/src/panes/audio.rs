#![allow(
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::if_not_else,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "PipeWire and wpctl are established product names"
)]
//! Audio pane — output volume + mute, microphone input level, device
//! capabilities, diagnostics, and per-application routing.
//!
//! Every control here drives real state:
//! - **Volume / Mute** forward to `os_actions` (`wpctl` on the default
//!   sink), the same bindings the compositor's quick-settings panel uses.
//! - **Input level** forwards to the default *source* via `wpctl`
//!   (`@DEFAULT_AUDIO_SOURCE@`) through [`InputControl`], sampled once on
//!   construction so the slider rests at the mic's current gain.
//!
//! Notification delivery sounds live in Notifications. Collaboration event
//! sounds are not offered here until the shell has a real consumer; Settings
//! never persists a switch that nothing applies.
//!
//! Device *selection* enumerates the real sinks/sources from `wpctl status`
//! ([`parse_wpctl_status`]); when more than one output (or input) device
//! exists a picker is shown, and choosing one runs `wpctl set-default <id>`
//! through [`DeviceControl`], persisting the choice to [`AudioPrefs`]. On a
//! host with no `PipeWire` the enumerator returns nothing, so the picker simply
//! isn't shown (no fabricated devices) and the volume/mute/input controls
//! still act on the current default.
//!
//! **Per-application routing** enumerates the `PipeWire` node graph via
//! [`media::audio`] (`pw-dump`): each application stream that is playing (or
//! recording) becomes a row with a picker that routes just that app to a chosen
//! device — the music app to the headphones while the browser stays on the
//! speakers — plus per-app volume and mute. Moving a stream writes the
//! `WirePlumber` `target.object` metadata; per-app volume/mute go through `wpctl`
//! on the stream's node id. The graph is re-read on a throttle so a newly
//! plugged headset (or a freshly started app) appears without reopening
//! Settings. Empty means empty: no application playing ⇒ no fabricated rows.
//
// CRDT keys the per-app section drives (keyed by PipeWire node id):
//   `audio.stream.<id>.sink`   — chosen output/input label, or "Default"
//   `audio.stream.<id>.volume` — f64 in `[0.0, 1.0]`, per-app gain
//   `audio.stream.<id>.muted`  — bool, per-app mute
//
// CRDT keys this pane reads / writes:
//   `audio.volume`            — f64 in `[0.0, 1.0]`, output slider
//   `audio.muted`             — bool, mute toggle
//   `audio.input_volume`      — f64 in `[0.0, 1.0]`, input slider
//   `audio.output_device`     — string, chosen sink name (when >1 sink)
//   `audio.input_device`      — string, chosen source name (when >1 source)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use ui::doc::{read_bool, read_f64, read_str_or_text};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use media::audio::{
    AudioEnumerator, AudioGraph, AudioStream, GraphControl, NoopGraphControl, RealEnumerator,
    RealGraphControl,
};
use media::audio_runtime::{
    AudioRuntimeControl, BoundedSession, MicrophoneMeterSession, RealAudioRuntime,
    RuntimeCapabilities,
};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Row, Slider, Spacer, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::dispatcher::dispatcher_or_real;
use crate::i18n::{t, t_args};
use crate::prefs::{AudioPrefs, SystemPrefs};
use crate::widgets::{caption, card, choice_row_described, rows, setting_row};
use media::event_sound::SoundTheme;
use settings_store::system_sounds::{SystemSoundEvent, SystemSoundPrefs};

/// How often the pane re-reads the `PipeWire` graph while it is open, so a
/// device plugged in (a headset) or an application that just started playing
/// appears without reopening Settings. Cheap enough for an actively-viewed
/// pane; the enumeration is one `pw-dump`.
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(1_500);

/// CRDT key prefix for a per-application stream's routing/volume/mute, keyed by
/// its `PipeWire` node id. `audio.stream.<id>.sink` holds the chosen output
/// label (or [`FOLLOW_DEFAULT`]); `.volume` the perceptual gain; `.muted` the
/// mute.
pub(crate) fn stream_sink_key(id: u32) -> String {
    format!("audio.stream.{id}.sink")
}
pub(crate) fn stream_volume_key(id: u32) -> String {
    format!("audio.stream.{id}.volume")
}
pub(crate) fn stream_muted_key(id: u32) -> String {
    format!("audio.stream.{id}.muted")
}
pub(crate) fn device_profile_key(id: u32) -> String {
    format!("audio.device.{id}.profile")
}

/// The picker entry meaning "no explicit device — follow the default". Choosing
/// it clears the stream's `target.object` metadata.
pub(crate) const FOLLOW_DEFAULT: &str = "Default";

fn follow_shown() -> String {
    t("sound-follow-default")
}

fn is_follow_default(choice: &str) -> bool {
    choice == FOLLOW_DEFAULT || choice == follow_shown()
}

fn event_shown(event: SystemSoundEvent) -> String {
    t(match event {
        SystemSoundEvent::Notification => "sound-event-notification",
        SystemSoundEvent::Message => "sound-event-message",
        SystemSoundEvent::VolumeChange => "sound-event-volume",
        SystemSoundEvent::Screenshot => "sound-event-screenshot",
        SystemSoundEvent::DeviceConnected => "sound-event-connected",
        SystemSoundEvent::DeviceDisconnected => "sound-event-disconnected",
        SystemSoundEvent::LowBattery => "sound-event-battery",
        SystemSoundEvent::Error => "sound-event-error",
    })
}

pub const VOLUME_KEY: &str = "audio.volume";
pub const MUTED_KEY: &str = "audio.muted";
pub const INPUT_VOLUME_KEY: &str = "audio.input_volume";
/// CRDT key the output-device (sink) dropdown writes the chosen name into.
pub const OUTPUT_DEVICE_KEY: &str = "audio.output_device";
/// CRDT key the input-device (source) dropdown writes the chosen name into.
pub const INPUT_DEVICE_KEY: &str = "audio.input_device";
pub const BALANCE_KEY: &str = "audio.balance";
pub const MONO_KEY: &str = "audio.mono";
pub const ECHO_CANCELLATION_KEY: &str = "audio.echo_cancellation";
pub const SYSTEM_SOUNDS_KEY: &str = "audio.system_sounds";
pub const SYSTEM_SOUND_VOLUME_KEY: &str = "audio.system_sound_volume";
/// CRDT key the sound-theme dropdown writes the chosen theme's name into.
pub const SOUND_THEME_KEY: &str = "audio.sound_theme";

/// CRDT key of one event's "makes a sound" toggle.
pub fn event_sound_key(event: SystemSoundEvent) -> String {
    format!("audio.event_sound.{}", event.key())
}
pub(crate) const ADVANCED_KEY: &str = "audio.advanced";
pub(crate) const TEST_LEFT_KEY: &str = "audio.test.left";
pub(crate) const TEST_RIGHT_KEY: &str = "audio.test.right";
pub(crate) const MIC_TEST_KEY: &str = "audio.mic_test";

struct NoopAudioRuntime;
impl AudioRuntimeControl for NoopAudioRuntime {
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }
    fn set_balance(&self, _: &str, _: f32, _: f32) -> Result<(), String> {
        Err("balance unavailable".into())
    }
    fn start_mono(&self, _: &str) -> Result<Box<dyn BoundedSession>, String> {
        Err("mono unavailable".into())
    }
    fn start_channel_test(&self, _: &str, _: bool) -> Result<Box<dyn BoundedSession>, String> {
        Err("channel test unavailable".into())
    }
    fn start_microphone_meter(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
        Err("microphone test unavailable".into())
    }
}

/// Side-effect interface for the microphone (default *source*) gain.
/// `os_actions::Dispatcher` only covers the sink, so the pane carries this
/// tiny extra seam; the production impl shells out to `wpctl`, tests inject
/// a recorder.
pub trait InputControl: Send + Sync {
    /// # Errors
    /// Returns the backend's error string when the `wpctl` call to set the
    /// default source's gain fails (missing binary, no default source, …).
    fn set_input_volume(&self, pct: f32) -> Result<(), String>;
}

/// Production impl: `wpctl set-volume @DEFAULT_AUDIO_SOURCE@ <fraction>`.
pub struct RealInput;

impl InputControl for RealInput {
    fn set_input_volume(&self, pct: f32) -> Result<(), String> {
        let frac = (pct.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
        run_wpctl(&[
            "set-volume",
            "@DEFAULT_AUDIO_SOURCE@",
            &format!("{frac:.2}"),
        ])
    }
}

/// Best-effort `wpctl` invocation. Returns the daemon's error string when
/// the binary is missing or the call fails, mirroring `os_actions`.
fn run_wpctl(args: &[&str]) -> Result<(), String> {
    let out = bounded_command::run("wpctl", args, None, 1024 * 1024, Duration::from_secs(5))
        .map_err(|error| format!("wpctl control failed: {error}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .chars()
            .filter(|character| !character.is_control() || matches!(character, ' ' | '\n' | '\t'))
            .take(512)
            .collect::<String>()
            .trim()
            .to_string())
    }
}

/// One enumerated audio device (a sink or a source) as reported by
/// `wpctl status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// `PipeWire` node id — the handle `wpctl set-default <id>` takes.
    pub id: u32,
    pub name: String,
    /// Whether this device is the current system default for its class.
    pub default: bool,
}

/// The enumerated sinks (outputs) and sources (inputs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioDevices {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
}

impl AudioDevices {
    /// Name of the current default sink, if any.
    pub fn default_sink(&self) -> Option<&str> {
        self.sinks
            .iter()
            .find(|d| d.default)
            .map(|d| d.name.as_str())
    }
    /// Name of the current default source, if any.
    pub fn default_source(&self) -> Option<&str> {
        self.sources
            .iter()
            .find(|d| d.default)
            .map(|d| d.name.as_str())
    }
    fn sink_id(&self, name: &str) -> Option<u32> {
        self.sinks.iter().find(|d| d.name == name).map(|d| d.id)
    }
    fn source_id(&self, name: &str) -> Option<u32> {
        self.sources.iter().find(|d| d.name == name).map(|d| d.id)
    }
}

/// Trim the leading tree/box-drawing glyphs (and whitespace) `wpctl status`
/// indents its rows with, leaving the row's content.
fn strip_tree_prefix(line: &str) -> &str {
    line.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─' | '┌' | '┐' | '┘' | '┬' | '┴' | '┼')
    })
}

/// Parse `wpctl status` into the enumerated sinks + sources. Robust against
/// the surrounding tree layout: it tracks the current `Sinks:` / `Sources:`
/// section and reads each `[*] <id>. <name> [vol: …]` row within it, ignoring
/// the Devices/Filters/Streams/Video sections. The `*` marks the default.
pub fn parse_wpctl_status(stdout: &str) -> AudioDevices {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        None,
        Sinks,
        Sources,
    }
    let mut section = Section::None;
    let mut out = AudioDevices::default();
    for raw in stdout.lines() {
        let content = strip_tree_prefix(raw);
        if content.is_empty() {
            continue;
        }
        // Section headers.
        if content == "Sinks:" {
            section = Section::Sinks;
            continue;
        }
        if content == "Sources:" {
            section = Section::Sources;
            continue;
        }
        // Any other header (Devices:/Filters:/Streams:/Clients:/Settings:) or a
        // column-0 category ("Audio"/"Video"/"PipeWire …") ends the section.
        let indented = raw.starts_with(|c: char| {
            c.is_whitespace()
                || matches!(c, '│' | '├' | '└' | '─' | '┌' | '┐' | '┘' | '┬' | '┴' | '┼')
        });
        if content.ends_with(':') || !indented {
            section = Section::None;
            continue;
        }
        if section == Section::None {
            continue;
        }
        // Device row: optional '*' (default), then "<id>. <name> [vol: …]".
        let (default, rest) = match content.strip_prefix('*') {
            Some(r) => (true, r.trim_start()),
            None => (false, content),
        };
        let Some((id_str, name_part)) = rest.split_once('.') else {
            continue;
        };
        let Ok(id) = id_str.trim().parse::<u32>() else {
            continue;
        };
        // Drop the trailing "[vol: …]" annotation.
        let mut name = name_part.trim().to_string();
        if let Some(idx) = name.rfind('[')
            && let Some(head) = name.get(..idx)
        {
            name = head.trim().to_string();
        }
        if name.is_empty() {
            continue;
        }
        let dev = AudioDevice { id, name, default };
        match section {
            Section::Sinks => out.sinks.push(dev),
            Section::Sources => out.sources.push(dev),
            Section::None => {}
        }
    }
    out
}

/// Enumerate the audio devices via `wpctl status`. Best-effort: an empty set
/// when `wpctl` is missing or fails, so a non-Tilo host with no `PipeWire` shows
/// no device pickers rather than fabricated devices.
fn list_audio_devices() -> AudioDevices {
    let Ok(out) = bounded_command::run(
        "wpctl",
        &["status"],
        None,
        1024 * 1024,
        Duration::from_secs(5),
    ) else {
        return AudioDevices::default();
    };
    if !out.status.success() {
        return AudioDevices::default();
    }
    parse_wpctl_status(&String::from_utf8_lossy(&out.stdout))
}

/// Side-effect interface for switching the default audio device. Production
/// runs `wpctl set-default <id>`; tests inject a recorder.
pub trait DeviceControl: Send + Sync {
    /// # Errors
    /// Returns the backend's error string when `wpctl set-default` fails
    /// (missing binary, unknown device id, …).
    fn set_default(&self, id: u32) -> Result<(), String>;
}

/// Production impl: `wpctl set-default <id>`.
pub struct RealDevice;

impl DeviceControl for RealDevice {
    fn set_default(&self, id: u32) -> Result<(), String> {
        run_wpctl(&["set-default", &id.to_string()])
    }
}

/// A no-op [`DeviceControl`] used where no device set is injected (the volume
/// change-guard tests), so those tests never touch `wpctl`.
struct NoopDevice;

impl DeviceControl for NoopDevice {
    fn set_default(&self, _id: u32) -> Result<(), String> {
        Ok(())
    }
}

/// Read the default source gain as a `0.0..=1.0` fraction, so the input
/// slider rests at the current source level. Best-effort; `None` on any
/// failure (no `wpctl`, parse error) so callers fall back to a default.
fn get_source_volume() -> Option<f32> {
    let out = bounded_command::run(
        "wpctl",
        &["get-volume", "@DEFAULT_AUDIO_SOURCE@"],
        None,
        1024 * 1024,
        Duration::from_secs(5),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `Volume: 0.42` (optionally ` [MUTED]`).
    let after = stdout.lines().next()?.split(':').nth(1)?.trim();
    after
        .split_whitespace()
        .next()?
        .parse::<f32>()
        .ok()
        .map(|v| v.clamp(0.0, 1.0))
}

/// Audio pane widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent UI/session flag (advanced-panel toggle, dirty flag, \
              last-value cache, test-only persistence switch) mutated at different times, not \
              a state machine that bitflags or an enum would clarify"
)]
pub struct AudioPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    input: Arc<dyn InputControl>,
    device: Arc<dyn DeviceControl>,
    devices: AudioDevices,
    /// The `PipeWire` node graph: the per-application streams the "Applications"
    /// / "Recording" cards route, plus the devices they can be routed to.
    graph: AudioGraph,
    /// Side-effect surface for editing the graph (route a stream, per-app
    /// volume/mute). Production shells out; tests inject a recorder.
    gctl: Arc<dyn GraphControl>,
    /// Source of fresh graph snapshots for hotplug re-enumeration.
    enumerator: Arc<dyn AudioEnumerator>,
    /// How often to re-enumerate the graph (hotplug). `MAX` disables it (test /
    /// shots constructors with a static graph).
    hotplug_interval: Duration,
    /// When the graph was last re-enumerated.
    last_enum: Option<Instant>,
    /// Streams whose CRDT keys have been seeded, so a re-seed doesn't re-fire a
    /// route/volume/mute.
    seeded_streams: HashSet<u32>,
    /// Per-stream last-dispatched values, guarding re-dispatch at rest.
    last_stream_sink: HashMap<u32, String>,
    last_stream_vol: HashMap<u32, f32>,
    last_stream_mute: HashMap<u32, bool>,
    last_device_profile: HashMap<u32, String>,
    runtime: Arc<dyn AudioRuntimeControl>,
    runtime_caps: RuntimeCapabilities,
    balance: f32,
    mono_enabled: bool,
    echo_cancellation: bool,
    system_sounds: SystemSoundPrefs,
    /// Installed freedesktop sound themes, for the theme picker.
    sound_themes: Vec<SoundTheme>,
    /// Whether this machine can play event sounds at all.
    event_sounds_available: bool,
    advanced: bool,
    runtime_notice: Option<String>,
    channel_test: Option<Box<dyn BoundedSession>>,
    mic_session: Option<Box<dyn MicrophoneMeterSession>>,
    mic_level: f32,
    structure_dirty: bool,
    initial_volume: f32,
    initial_input: f32,
    initial_prefs: AudioPrefs,
    last_volume: f32,
    last_input: f32,
    last_muted: bool,
    /// Name of the default sink we last set/seeded, guarding re-dispatch.
    last_sink: Option<String>,
    /// Name of the default source we last set/seeded, guarding re-dispatch.
    last_source: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    require_operation_token: bool,
    /// Whether device/sound changes persist to the shared prefs store. Off in
    /// the test/shots constructors so the suite never writes a real
    /// `settings.json`.
    persist_prefs: bool,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

/// A static [`AudioEnumerator`] that always yields the same graph — used by the
/// test/shots constructors so re-enumeration (when enabled) is deterministic
/// and never touches `pw-dump`.
struct StaticEnumerator(AudioGraph);
impl AudioEnumerator for StaticEnumerator {
    fn enumerate(&self) -> AudioGraph {
        self.0.clone()
    }
}

/// Map the media crate's graph devices into the pane's local [`AudioDevices`]
/// view (id + human label + default flag) so a hotplug re-enumeration refreshes
/// the sink/source pickers the same way the initial `wpctl` scan seeded them.
fn devices_from_graph(graph: &AudioGraph) -> AudioDevices {
    let map = |ds: &[media::audio::AudioDevice]| {
        ds.iter()
            .map(|d| AudioDevice {
                id: d.id,
                name: d.description.clone(),
                default: d.is_default,
            })
            .collect()
    };
    AudioDevices {
        sinks: map(&graph.sinks),
        sources: map(&graph.sources),
    }
}

impl Default for AudioPane {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPane {
    pub fn new() -> Self {
        let initial_state = os_actions::get_volume_state().unwrap_or(os_actions::VolumeState {
            level: 0.5,
            muted: false,
        });
        let initial = initial_state.level;
        let initial_input = get_source_volume().unwrap_or(0.75);
        let prefs = SystemPrefs::load().audio;
        let mut devices = list_audio_devices();
        // Enumerate the PipeWire graph for the per-application streams (and the
        // devices they route to). Empty on a host with no PipeWire, so the
        // Applications card simply shows its empty state.
        let mut graph = media::audio::enumerate();
        let runtime: Arc<dyn AudioRuntimeControl> = Arc::new(RealAudioRuntime);
        let runtime_caps = runtime.capabilities();
        // Re-apply a remembered physical route when it is present. The action
        // is made against the current PipeWire node id; the persisted value is
        // only a preference; it is applied only when the node is present.
        if let Some(name) = prefs.output_device.as_deref()
            && let Some(dev) = graph.sinks.iter().find(|d| d.description == name)
        {
            let id = dev.id;
            if RealGraphControl.set_default_sink(id).is_ok() {
                for sink in &mut graph.sinks {
                    sink.is_default = sink.id == id;
                }
            }
        }
        if let Some(name) = prefs.input_device.as_deref()
            && let Some(dev) = graph.sources.iter().find(|d| d.description == name)
        {
            let id = dev.id;
            if RealGraphControl.set_default_source(id).is_ok() {
                for source in &mut graph.sources {
                    source.is_default = source.id == id;
                }
            }
        }
        if !graph.sinks.is_empty() || !graph.sources.is_empty() {
            devices = devices_from_graph(&graph);
        }
        let last_sink = devices.default_sink().map(str::to_string);
        let last_source = devices.default_source().map(str::to_string);
        Self {
            dispatcher: dispatcher_or_real(None),
            input: Arc::new(RealInput),
            device: Arc::new(RealDevice),
            devices,
            graph,
            gctl: Arc::new(RealGraphControl),
            enumerator: Arc::new(RealEnumerator),
            hotplug_interval: HOTPLUG_INTERVAL,
            last_enum: Some(ui::time::now()),
            seeded_streams: HashSet::new(),
            last_stream_sink: HashMap::new(),
            last_stream_vol: HashMap::new(),
            last_stream_mute: HashMap::new(),
            last_device_profile: HashMap::new(),
            runtime,
            runtime_caps,
            balance: prefs.balance.clamp(0.0, 1.0),
            mono_enabled: prefs.mono_output,
            echo_cancellation: prefs.echo_cancellation,
            system_sounds: prefs.system_sounds.clone(),
            sound_themes: media::event_sound::installed_themes(),
            event_sounds_available: media::event_sound::sound_backend()
                != media::event_sound::SoundBackend::Unavailable,
            advanced: false,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_level: 0.0,
            structure_dirty: false,
            initial_volume: initial,
            initial_input,
            initial_prefs: prefs,
            last_volume: initial,
            last_input: initial_input,
            last_muted: initial_state.muted,
            last_sink,
            last_source,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            persist_prefs: true,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_volume: f32,
    ) -> Self {
        Self::with_controls(dispatcher, Arc::new(RealInput), initial_volume, 0.75)
    }

    /// Full test constructor: inject both the sink dispatcher and the
    /// microphone `InputControl`, plus deterministic initial gains so the
    /// change-guard tests don't depend on the host's audio devices.
    pub fn with_controls(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        input: Arc<dyn InputControl>,
        initial_volume: f32,
        initial_input: f32,
    ) -> Self {
        Self {
            dispatcher,
            input,
            device: Arc::new(NoopDevice),
            devices: AudioDevices::default(),
            graph: AudioGraph::default(),
            gctl: Arc::new(NoopGraphControl),
            enumerator: Arc::new(StaticEnumerator(AudioGraph::default())),
            hotplug_interval: Duration::MAX,
            last_enum: Some(ui::time::now()),
            seeded_streams: HashSet::new(),
            last_stream_sink: HashMap::new(),
            last_stream_vol: HashMap::new(),
            last_stream_mute: HashMap::new(),
            last_device_profile: HashMap::new(),
            runtime: Arc::new(NoopAudioRuntime),
            runtime_caps: RuntimeCapabilities::default(),
            balance: 0.5,
            mono_enabled: false,
            echo_cancellation: AudioPrefs::default().echo_cancellation,
            system_sounds: SystemSoundPrefs::default(),
            sound_themes: Vec::new(),
            event_sounds_available: false,
            advanced: false,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_level: 0.0,
            structure_dirty: false,
            initial_volume,
            initial_input,
            initial_prefs: AudioPrefs::default(),
            last_volume: initial_volume,
            last_input: initial_input,
            last_muted: false,
            last_sink: None,
            last_source: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            persist_prefs: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` builder: inject an enumerated device set and the
    /// [`DeviceControl`] recorder so the sink/source picker path is exercised
    /// without shelling out to `wpctl`.
    #[must_use]
    pub fn with_devices(mut self, devices: AudioDevices, device: Arc<dyn DeviceControl>) -> Self {
        self.last_sink = devices.default_sink().map(str::to_string);
        self.last_source = devices.default_source().map(str::to_string);
        self.devices = devices;
        self.device = device;
        self
    }

    /// Test / `--shots` builder: inject a `PipeWire` [`AudioGraph`] and the
    /// [`GraphControl`] recorder so the per-application routing path is
    /// exercised without touching `pw-dump` / `pw-metadata`. A static
    /// enumerator returns the same graph, so any hotplug re-enumeration is a
    /// no-op unless [`Self::with_hotplug`] scripts one.
    #[must_use]
    pub fn with_graph(mut self, graph: AudioGraph, gctl: Arc<dyn GraphControl>) -> Self {
        self.enumerator = Arc::new(StaticEnumerator(graph.clone()));
        self.graph = graph;
        self.gctl = gctl;
        self
    }

    /// Test builder: a machine that can play event sounds from `themes`.
    #[cfg(test)]
    #[must_use]
    fn with_sound_themes(mut self, themes: Vec<SoundTheme>) -> Self {
        self.sound_themes = themes;
        self.event_sounds_available = true;
        self
    }

    #[must_use]
    pub fn with_runtime(mut self, runtime: Arc<dyn AudioRuntimeControl>) -> Self {
        self.runtime_caps = runtime.capabilities();
        self.runtime = runtime;
        self
    }

    /// Test builder: drive the hotplug path from a scripted enumerator with the
    /// throttle disabled, so every `sync` re-enumerates and a device/stream
    /// added to a later snapshot is picked up.
    #[must_use]
    pub fn with_hotplug(mut self, enumerator: Arc<dyn AudioEnumerator>) -> Self {
        self.enumerator = enumerator;
        self.hotplug_interval = Duration::ZERO;
        self.last_enum = None;
        self
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self.require_operation_token = true;
        self
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    fn execute_local_command(
        &mut self,
        local: &mut AutoCommit,
        operation: crate::actions::AudioOperation,
    ) {
        let root = automerge::ROOT;
        use crate::actions::AudioOperation as Op;
        match &operation {
            Op::SetOutputVolume { value } => {
                let _ = local.put(&root, VOLUME_KEY, *value);
            }
            Op::SetOutputMuted { muted } => {
                let _ = local.put(&root, MUTED_KEY, *muted);
            }
            Op::SetInputVolume { value } => {
                let _ = local.put(&root, INPUT_VOLUME_KEY, *value);
            }
            Op::SetBalance { value } => {
                let _ = local.put(&root, BALANCE_KEY, *value);
            }
            Op::SetMono { enabled } => {
                let _ = local.put(&root, MONO_KEY, *enabled);
            }
            Op::SetEchoCancellation { enabled } => {
                let _ = local.put(&root, ECHO_CANCELLATION_KEY, *enabled);
            }
            Op::SetSystemSounds { enabled } => {
                let _ = local.put(&root, SYSTEM_SOUNDS_KEY, *enabled);
            }
            Op::SetSystemSoundVolume { value } => {
                let _ = local.put(&root, SYSTEM_SOUND_VOLUME_KEY, *value);
            }
            Op::SetSoundTheme { theme } => {
                if let Some(installed) = self.sound_themes.iter().find(|t| &t.id == theme) {
                    let _ = local.put(&root, SOUND_THEME_KEY, installed.name.as_str());
                }
            }
            Op::SetEventSound { event, enabled } => {
                if let Some(event) = SystemSoundEvent::from_key(event) {
                    let _ = local.put(&root, event_sound_key(event), *enabled);
                }
            }
            Op::SetDeviceProfile {
                device_id,
                profile_index,
            } => {
                if let Some(profile) = self
                    .graph
                    .hardware
                    .iter()
                    .find(|d| d.id == *device_id)
                    .and_then(|d| d.profiles.iter().find(|p| p.index == *profile_index))
                {
                    let _ = local.put(
                        &root,
                        device_profile_key(*device_id),
                        profile.description.as_str(),
                    );
                }
            }
            Op::RouteStream {
                stream_id,
                playback,
                device_id,
            } => {
                let devices = if *playback {
                    &self.graph.sinks
                } else {
                    &self.graph.sources
                };
                let label = device_id
                    .and_then(|id| {
                        devices
                            .iter()
                            .find(|d| d.id == id)
                            .map(|d| d.description.clone())
                    })
                    .unwrap_or_else(|| FOLLOW_DEFAULT.into());
                let _ = local.put(&root, stream_sink_key(*stream_id), label);
            }
            Op::SetStreamVolume { stream_id, value } => {
                let _ = local.put(&root, stream_volume_key(*stream_id), *value);
            }
            Op::SetStreamMuted { stream_id, muted } => {
                let _ = local.put(&root, stream_muted_key(*stream_id), *muted);
            }
            Op::ToggleDiagnostics => {
                let _ = local.put(&root, ADVANCED_KEY, !self.advanced);
            }
            Op::TestChannel { left } => {
                let _ = local.put(
                    &root,
                    if *left { TEST_LEFT_KEY } else { TEST_RIGHT_KEY },
                    true,
                );
            }
            Op::ToggleMicrophoneMeter => {
                let _ = local.put(&root, MIC_TEST_KEY, true);
            }
            Op::SelectOutput { device_id } => {
                if let Some(d) = self.graph.sink_by_id(*device_id) {
                    let _ = local.put(&root, OUTPUT_DEVICE_KEY, d.description.as_str());
                }
            }
            Op::SelectInput { device_id } => {
                if let Some(d) = self.graph.sources.iter().find(|d| d.id == *device_id) {
                    let _ = local.put(&root, INPUT_DEVICE_KEY, d.description.as_str());
                }
            }
            Op::RoutePlayback {
                stream_id,
                device_id,
            } => {
                if let Some(d) = self.graph.sink_by_id(*device_id) {
                    let _ = local.put(&root, stream_sink_key(*stream_id), d.description.as_str());
                }
            }
            Op::RouteRecording {
                stream_id,
                device_id,
            } => {
                if let Some(d) = self.graph.sources.iter().find(|d| d.id == *device_id) {
                    let _ = local.put(&root, stream_sink_key(*stream_id), d.description.as_str());
                }
            }
        }
        self.apply_volume_controls(local, &root);
        self.apply_balance_and_mono(local, &root);
        self.apply_echo_cancellation(local, &root);
        self.apply_system_sounds(local, &root);
        self.apply_diagnostics_requests(local, &root);
        self.apply_device_selection(local, &root);
        self.apply_streams_and_profiles(local, &root);
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers construct the exact typed request whose token is completed"
    )]
    fn finish_local_operation(
        &self,
        operation: crate::actions::AudioOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::AUDIO_OPERATION_TYPE,
            &operation,
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    fn has_pending_operation(&self) -> bool {
        if !self.require_operation_token {
            return true;
        }
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token }
                if token.starts_with(crate::actions::AUDIO_OPERATION_TYPE)
        )
    }

    fn operation_is_running(&self, operation: &crate::actions::AudioOperation) -> bool {
        if !self.require_operation_token {
            return true;
        }
        let token = crate::actions::local_operation_token_for(
            crate::actions::AUDIO_OPERATION_TYPE,
            operation,
        );
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        )
    }

    /// The enumerated audio devices (tests read this to assert rendering).
    pub fn devices(&self) -> &AudioDevices {
        &self.devices
    }

    /// The enumerated `PipeWire` graph (tests read this to assert hotplug
    /// re-enumeration landed).
    pub fn graph(&self) -> &AudioGraph {
        &self.graph
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let output = self.build_output_card();
        let input = self.build_input_card();
        let hardware = self.build_hardware_card();

        // --- Applications card: one block per playback stream, with a picker
        //     that routes just that app to a chosen sink, plus per-app volume +
        //     mute. This is the per-application routing surface — the music app
        //     to the headphones while the browser stays on the speakers.
        let apps = self.streams_card(
            &t("sound-apps"),
            &self.graph.playback_streams,
            &self.devices.sinks,
            &t("sound-apps-empty"),
        );
        // --- Recording card: the capture streams (which app is using the mic),
        //     routed to a chosen source, with per-app level + mute.
        let recording = self.streams_card(
            &t("sound-recording"),
            &self.graph.record_streams,
            &self.devices.sources,
            &t("sound-recording-empty"),
        );

        let mut cards = vec![output, input];
        if let Some(hardware) = hardware {
            cards.push(hardware);
        }
        cards.extend([
            apps,
            recording,
            Self::speakers_card(),
            self.build_system_sounds_card(),
        ]);
        crate::widgets::cards(cards)
    }

    /// The way into Speakers: several of your devices as one set.
    fn speakers_card() -> crate::widgets::El {
        let open_label = t("sound-speakers-open");
        let open = ui::widget::Button::new("sound.speakers", &open_label, None).content_sized();
        let open = match tilo_actions::ActionBinding::unit_for(
            &crate::actions::pane_navigation_resource(crate::categories::Category::Speakers),
            crate::actions::OPEN_PANE_ACTION,
        ) {
            Ok(binding) => open.on_action(binding),
            Err(_) => open.with_enabled(false),
        };
        crate::widgets::card(
            &t("sound-speakers-title"),
            crate::widgets::setting_row_described(
                &t("sound-speakers-row"),
                &t("sound-speakers-desc"),
                Box::new(open),
            ),
        )
    }

    /// The "Output" card: an optional device picker (only when more than one
    /// sink exists), then volume + mute on the default sink, plus balance /
    /// mono / speaker-test controls the runtime reports support for.
    fn build_output_card(&self) -> crate::widgets::El {
        let mut output_rows = Vec::new();
        if self.devices.sinks.len() >= 2 {
            let opts: Vec<String> = self.devices.sinks.iter().map(|d| d.name.clone()).collect();
            let mut dd = Dropdown::new(opts, OUTPUT_DEVICE_KEY).with_id("audio.output_device");
            let devices = self
                .devices
                .sinks
                .iter()
                .map(|device| (device.name.clone(), device.id))
                .collect();
            if let Some(binding) = crate::actions::audio_device_binding("output-device", devices) {
                dd = dd.on_action(binding);
            } else {
                dd = dd.with_enabled(false);
            }
            if let Some(name) = self.devices.default_sink() {
                dd = dd.with_placeholder(name);
            }
            output_rows.push(setting_row(&t("sound-output-device"), Box::new(dd)));
        }
        output_rows.push(setting_row(
            &t("sound-volume"),
            Box::new(
                Slider::new("settings.audio.volume", VOLUME_KEY)
                    .with_id("audio.volume")
                    .on_action(
                        crate::actions::audio_number_binding("volume")
                            .expect("output volume is part of the static resource contract"),
                    ),
            ),
        ));
        output_rows.push(setting_row(
            &t("sound-mute"),
            Box::new(
                Toggle::new("settings.audio.mute", MUTED_KEY)
                    .with_id("audio.mute")
                    .on_action(
                        crate::actions::audio_bool_binding("muted")
                            .expect("output mute is part of the static resource contract"),
                    ),
            ),
        ));
        let stereo_sink = self.graph.default_sink().filter(|sink| sink.channels == 2);
        if self.runtime_caps.balance && stereo_sink.is_some() {
            output_rows.push(setting_row(
                &t("sound-balance"),
                Box::new(
                    Slider::new("settings.audio.balance", BALANCE_KEY)
                        .with_id("audio.balance")
                        .on_action(
                            crate::actions::audio_number_binding("balance")
                                .expect("output balance is part of the static resource contract"),
                        ),
                ),
            ));
        }
        if self.runtime_caps.mono_output && stereo_sink.is_some() {
            output_rows.push(setting_row(
                &t("sound-mono"),
                Box::new(
                    Toggle::new("settings.audio.mono", MONO_KEY)
                        .with_id("audio.mono")
                        .on_action(
                            crate::actions::audio_bool_binding("mono")
                                .expect("mono output is part of the static resource contract"),
                        ),
                ),
            ));
            output_rows.push(caption(&t("sound-mono-caption")));
        }
        if self.runtime_caps.channel_test && stereo_sink.is_some() {
            output_rows.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new(
                            "settings.audio.advanced",
                            &if self.advanced {
                                t("sound-test-hide")
                            } else {
                                t("sound-test-show")
                            },
                            None,
                        )
                        .content_sized()
                        .on_action(
                            crate::actions::audio_unit_binding("diagnostics")
                                .expect("audio diagnostics action is published"),
                        ),
                    ))
                    .push(Box::new(Spacer::new())),
            ));
            if self.advanced {
                output_rows.push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Button::new("settings.audio.test.left", &t("sound-test-left"), None)
                                .content_sized()
                                .on_action(
                                    crate::actions::audio_unit_binding("test-left")
                                        .expect("left speaker test action is published"),
                                ),
                        ))
                        .push(Box::new(
                            Button::new("settings.audio.test.right", &t("sound-test-right"), None)
                                .content_sized()
                                .on_action(
                                    crate::actions::audio_unit_binding("test-right")
                                        .expect("right speaker test action is published"),
                                ),
                        ))
                        .push(Box::new(Spacer::new())),
                ));
                output_rows.push(caption(&t("sound-test-caption")));
            }
        }
        if let Some(notice) = &self.runtime_notice {
            output_rows.push(caption(notice));
        }
        card(&t("sound-output"), rows(output_rows))
    }

    /// The "Input" card: an optional source picker, then the mic level and
    /// (when the runtime supports it) a bounded microphone meter test.
    fn build_input_card(&self) -> crate::widgets::El {
        let mut input_rows = Vec::new();
        if self.devices.sources.len() >= 2 {
            let opts: Vec<String> = self
                .devices
                .sources
                .iter()
                .map(|d| d.name.clone())
                .collect();
            let mut dd = Dropdown::new(opts, INPUT_DEVICE_KEY).with_id("audio.input_device");
            let devices = self
                .devices
                .sources
                .iter()
                .map(|device| (device.name.clone(), device.id))
                .collect();
            if let Some(binding) = crate::actions::audio_device_binding("input-device", devices) {
                dd = dd.on_action(binding);
            } else {
                dd = dd.with_enabled(false);
            }
            if let Some(name) = self.devices.default_source() {
                dd = dd.with_placeholder(name);
            }
            input_rows.push(setting_row(&t("sound-input-device"), Box::new(dd)));
        }
        input_rows.push(setting_row(
            &t("sound-input-level"),
            Box::new(
                Slider::new("settings.audio.input_volume", INPUT_VOLUME_KEY)
                    .with_id("audio.input_volume")
                    .on_action(
                        crate::actions::audio_number_binding("input-volume")
                            .expect("input volume is part of the static resource contract"),
                    ),
            ),
        ));
        if self.runtime_caps.microphone_meter && self.graph.default_source().is_some() {
            let running = self.mic_session.is_some();
            let activity = if running {
                t_args(
                    "sound-input-activity",
                    &[(
                        "percent",
                        i64::from((self.mic_level * 100.0).round() as u32).into(),
                    )],
                )
            } else {
                t("sound-mic-idle")
            };
            input_rows.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&activity).with_size(12.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Button::new(
                            "settings.audio.mic_test",
                            &if running {
                                t("sound-mic-stop")
                            } else {
                                t("sound-mic-test")
                            },
                            None,
                        )
                        .content_sized()
                        .on_action(
                            crate::actions::audio_unit_binding("microphone-meter")
                                .expect("microphone meter action is published"),
                        ),
                    )),
            ));
            input_rows.push(caption(&t("sound-mic-caption")));
        }
        if self.runtime_caps.echo_cancellation && !self.graph.sources.is_empty() {
            input_rows.push(setting_row(
                &t("sound-echo"),
                Box::new(
                    Toggle::new("settings.audio.echo_cancellation", ECHO_CANCELLATION_KEY)
                        .with_id("audio.echo_cancellation")
                        .on_action(
                            crate::actions::audio_bool_binding("echo-cancellation").expect(
                                "echo cancellation is part of the static resource contract",
                            ),
                        ),
                ),
            ));
            input_rows.push(caption(&t("sound-echo-caption")));
        }
        card(&t("sound-input"), rows(input_rows))
    }

    /// The "Device capabilities" card: per-hardware profile pickers (only for
    /// devices reporting more than one selectable profile) plus each sink's
    /// reported codec. `None` when there is nothing to show — no empty card.
    fn build_hardware_card(&self) -> Option<crate::widgets::El> {
        let mut hardware_rows = Vec::new();
        for hw in &self.graph.hardware {
            let selectable: Vec<_> = hw.profiles.iter().filter(|p| p.available).collect();
            if selectable.len() < 2 {
                continue;
            }
            let options = selectable.iter().map(|p| p.description.clone()).collect();
            let profile_binding = crate::actions::audio_profile_binding(
                hw.id,
                selectable
                    .iter()
                    .map(|profile| (profile.description.clone(), profile.index))
                    .collect(),
            )
            .expect("visible audio profiles form a bounded resource action");
            let active = hw
                .active_profile
                .and_then(|i| hw.profiles.iter().find(|p| p.index == i))
                .map_or_else(|| t("sound-choose-profile"), |p| p.description.clone());
            hardware_rows.push(choice_row_described(
                &hw.description,
                &t("sound-profile"),
                Box::new(
                    Dropdown::new(options, &device_profile_key(hw.id))
                        .with_id(&format!("audio.profile.{}", hw.id))
                        .on_action(profile_binding)
                        .with_placeholder(&active),
                ),
            ));
        }
        for sink in &self.graph.sinks {
            if let Some(codec) = &sink.codec {
                hardware_rows.push(crate::widgets::value_row(
                    &t_args(
                        "sound-codec",
                        &[("device", sink.description.as_str().into())],
                    ),
                    codec,
                ));
            }
        }
        (!hardware_rows.is_empty()).then(|| card(&t("sound-hardware"), rows(hardware_rows)))
    }

    /// Build one streams card (Applications or Recording). Each stream becomes
    /// a routing picker (the app's target device, or "Default" to follow the
    /// system default) plus a volume slider and a mute toggle, all keyed by the
    /// stream's node id. An empty stream list renders `empty_note` rather than a
    /// fabricated row.
    fn streams_card(
        &self,
        title: &str,
        streams: &[AudioStream],
        devices: &[AudioDevice],
        empty_note: &str,
    ) -> crate::widgets::El {
        if streams.is_empty() {
            return card(title, rows(vec![caption(empty_note)]));
        }
        // Picker options: every device label, plus a leading "follow default".
        let mut options = vec![follow_shown()];
        options.extend(devices.iter().map(|d| d.name.clone()));

        let mut blocks: Vec<crate::widgets::El> = Vec::new();
        for s in streams {
            let sink_key = stream_sink_key(s.id);
            let vol_key = stream_volume_key(s.id);
            let mute_key = stream_muted_key(s.id);
            let current = self.stream_target_label(s);
            let route_devices = devices
                .iter()
                .map(|device| (device.name.clone(), device.id))
                .collect();
            let dd = Dropdown::new(options.clone(), &sink_key)
                .with_id(&format!("{sink_key}.dd"))
                .on_action(
                    crate::actions::audio_stream_route_binding(
                        s.id,
                        s.playback,
                        &s.label(),
                        route_devices,
                    )
                    .expect("visible stream routes form a bounded resource action"),
                )
                .with_placeholder(&current);
            let route_row =
                choice_row_described(&s.app, s.media_name.as_deref().unwrap_or(""), Box::new(dd));
            let vol_row = setting_row(
                &t("sound-volume"),
                Box::new(
                    Slider::new("settings.audio.volume", &vol_key)
                        .with_id(&format!("{vol_key}.sl"))
                        .on_action(
                            crate::actions::audio_stream_number_binding(
                                s.id,
                                s.playback,
                                &s.label(),
                            )
                            .expect("stream volume is part of its resource contract"),
                        ),
                ),
            );
            let mute_row = setting_row(
                &t("sound-mute"),
                Box::new(
                    Toggle::new("settings.audio.mute", &mute_key)
                        .with_id(&format!("{mute_key}.tg"))
                        .on_action(
                            crate::actions::audio_stream_bool_binding(s.id, s.playback, &s.label())
                                .expect("stream mute is part of its resource contract"),
                        ),
                ),
            );
            blocks.push(rows(vec![route_row, vol_row, mute_row]));
        }
        card(title, rows(blocks))
    }

    /// The picker label for a stream's *current* route: the device it is linked
    /// to, or "Default" when that device is the system default (so a stream on
    /// the default reads as following it, not pinned).
    fn stream_target_label(&self, s: &AudioStream) -> String {
        let devices = if s.playback {
            &self.graph.sinks
        } else {
            &self.graph.sources
        };
        match s
            .target_id
            .and_then(|tid| devices.iter().find(|d| d.id == tid))
        {
            Some(d) if d.is_default => follow_shown(),
            Some(d) => d.description.clone(),
            None => follow_shown(),
        }
    }

    /// Seed a stream's CRDT keys (route label / volume / mute) once, resting
    /// each guard on the seeded value so the seed itself dispatches nothing.
    fn seed_stream(&mut self, state: &mut AutoCommit, root: &ObjId, s: &AudioStream) {
        if self.seeded_streams.contains(&s.id) {
            return;
        }
        let label = self.stream_target_label(s);
        let vol = s.volume.unwrap_or(1.0);
        let muted = s.muted.unwrap_or(false);
        let _ = state.put(root, stream_sink_key(s.id).as_str(), label.clone());
        let _ = state.put(
            root,
            stream_volume_key(s.id).as_str(),
            automerge::ScalarValue::F64(f64::from(vol)),
        );
        let _ = state.put(
            root,
            stream_muted_key(s.id).as_str(),
            automerge::ScalarValue::Boolean(muted),
        );
        self.last_stream_sink.insert(s.id, label);
        self.last_stream_vol.insert(s.id, vol);
        self.last_stream_mute.insert(s.id, muted);
        self.seeded_streams.insert(s.id);
    }

    /// Apply a stream's routing/volume/mute from the CRDT, guarded so a value at
    /// rest doesn't re-fire. Routes through the injected [`GraphControl`].
    fn dispatch_stream(&mut self, state: &mut AutoCommit, root: &ObjId, s: &AudioStream) {
        // Route: match the chosen label to a device serial (or follow default).
        if let Some(choice) = read_str_or_text(state, root, stream_sink_key(s.id).as_str())
            && self.last_stream_sink.get(&s.id).map(String::as_str) != Some(choice.as_str())
        {
            let target_id = if is_follow_default(&choice) {
                None
            } else {
                let devices = if s.playback {
                    &self.graph.sinks
                } else {
                    &self.graph.sources
                };
                devices
                    .iter()
                    .find(|device| device.description == choice)
                    .map(|device| device.id)
            };
            let operation = crate::actions::AudioOperation::RouteStream {
                stream_id: s.id,
                playback: s.playback,
                device_id: target_id,
            };
            if !self.operation_is_running(&operation) {
                if let Some(previous) = self.last_stream_sink.get(&s.id) {
                    let _ = state.put(root, stream_sink_key(s.id), previous.as_str());
                }
                return;
            }
            let result = match target_id {
                None => self.gctl.unroute_stream(s.id),
                Some(device_id) => {
                    let devices = if s.playback {
                        &self.graph.sinks
                    } else {
                        &self.graph.sources
                    };
                    devices
                        .iter()
                        .find(|device| device.id == device_id)
                        .ok_or_else(|| "audio route is no longer available".to_string())
                        .and_then(|device| self.gctl.route_stream(s.id, device.serial))
                }
            };
            match result {
                Ok(()) => {
                    self.last_stream_sink.insert(s.id, choice);
                    self.finish_local_operation(operation, Ok(t("sound-notice-stream-route")));
                }
                Err(error) => {
                    if let Some(previous) = self.last_stream_sink.get(&s.id) {
                        let _ = state.put(root, stream_sink_key(s.id), previous.as_str());
                    }
                    warn!(target: "tilo::settings", op = "route_stream", stream = s.id, error = %error, "audio pane stream route failed");
                    self.finish_local_operation(
                        operation,
                        Err(t_args(
                            "sound-notice-stream-route-err",
                            &[("error", error.into())],
                        )),
                    );
                }
            }
        }
        // Per-app volume.
        if let Some(v) = read_f64(state, root, stream_volume_key(s.id).as_str()) {
            let v = v as f32;
            if (v - self.last_stream_vol.get(&s.id).copied().unwrap_or(v)).abs() > 0.001 {
                let operation = crate::actions::AudioOperation::SetStreamVolume {
                    stream_id: s.id,
                    value: f64::from(v),
                };
                if !self.operation_is_running(&operation) {
                    if let Some(previous) = self.last_stream_vol.get(&s.id) {
                        let _ = state.put(root, stream_volume_key(s.id), f64::from(*previous));
                    }
                    return;
                }
                match self.gctl.set_node_volume(s.id, v.clamp(0.0, 1.0)) {
                    Ok(()) => {
                        self.last_stream_vol.insert(s.id, v);
                        self.finish_local_operation(operation, Ok(t("sound-notice-stream-vol")));
                    }
                    Err(e) => {
                        if let Some(previous) = self.last_stream_vol.get(&s.id) {
                            let _ = state.put(root, stream_volume_key(s.id), f64::from(*previous));
                        }
                        self.finish_local_operation(
                            operation,
                            Err(t_args(
                                "sound-notice-stream-vol-err",
                                &[("error", e.clone().into())],
                            )),
                        );
                        warn!(target: "tilo::settings", op = "stream_volume", stream = s.id, error = %e, "audio pane stream volume failed");
                    }
                }
            }
        }
        // Per-app mute.
        if let Some(b) = read_bool(state, root, stream_muted_key(s.id).as_str())
            && self.last_stream_mute.get(&s.id).copied() != Some(b)
        {
            let operation = crate::actions::AudioOperation::SetStreamMuted {
                stream_id: s.id,
                muted: b,
            };
            if !self.operation_is_running(&operation) {
                if let Some(previous) = self.last_stream_mute.get(&s.id) {
                    let _ = state.put(root, stream_muted_key(s.id), *previous);
                }
                return;
            }
            match self.gctl.set_node_mute(s.id, b) {
                Ok(()) => {
                    self.last_stream_mute.insert(s.id, b);
                    self.finish_local_operation(operation, Ok(t("sound-notice-stream-mute")));
                }
                Err(e) => {
                    if let Some(previous) = self.last_stream_mute.get(&s.id) {
                        let _ = state.put(root, stream_muted_key(s.id), *previous);
                    }
                    self.finish_local_operation(
                        operation,
                        Err(t_args(
                            "sound-notice-stream-mute-err",
                            &[("error", e.clone().into())],
                        )),
                    );
                    warn!(target: "tilo::settings", op = "stream_mute", stream = s.id, error = %e, "audio pane stream mute failed");
                }
            }
        }
    }

    /// Re-enumerate the graph on the hotplug throttle. When the topology
    /// changed (a device plugged/unplugged, an app started/stopped playing),
    /// adopt the new graph, refresh the device pickers, and drop the composed
    /// tree so the next layout rebuilds with the new rows. Returns whether a
    /// structural change was adopted.
    fn poll_hotplug(&mut self) -> bool {
        let due = match (self.hotplug_interval, self.last_enum) {
            (i, _) if i == Duration::ZERO => true,
            (i, _) if i == Duration::MAX => false,
            (i, Some(t)) => t.elapsed() >= i,
            (_, None) => true,
        };
        if !due {
            return false;
        }
        self.last_enum = Some(ui::time::now());
        let fresh = self.enumerator.enumerate();
        if fresh.topology_signature() == self.graph.topology_signature() {
            return false;
        }
        // Forget streams that vanished so their guards don't leak.
        let present: HashSet<u32> = fresh
            .playback_streams
            .iter()
            .chain(fresh.record_streams.iter())
            .map(|s| s.id)
            .collect();
        self.seeded_streams.retain(|id| present.contains(id));
        self.last_stream_sink.retain(|id, _| present.contains(id));
        self.last_stream_vol.retain(|id, _| present.contains(id));
        self.last_stream_mute.retain(|id, _| present.contains(id));
        // Pane-owned diagnostics are bound to the old node name. Persistent
        // mono is owned by the compositor session and handles hot-plug there.
        self.channel_test = None;
        self.mic_session = None;
        // Refresh the sink/source pickers from the same enumeration so a new
        // headset is selectable, keeping the current default marker.
        self.devices = devices_from_graph(&fresh);
        self.graph = fresh;
        self.composed = None;
        true
    }

    /// Load the current prefs from disk, apply `f` to the audio section, and
    /// persist — reading fresh each time so a change here never clobbers a
    /// different pane's section.
    fn persist_audio(f: impl FnOnce(&mut AudioPrefs)) -> Result<(), String> {
        let mut p = SystemPrefs::load();
        f(&mut p.audio);
        p.save().map(|_| ()).map_err(|e| {
            warn!(target: "tilo::settings", op = "audio_prefs_save", error = %e, "failed to persist audio prefs");
            t_args("sound-notice-pref-err", &[("error", e.to_string().into())])
        })
    }

    /// Advance the bounded speaker/mic diagnostics and re-enumerate the
    /// `PipeWire` graph on the hotplug throttle.
    fn tick_sessions_and_hotplug(&mut self) {
        if self
            .channel_test
            .as_mut()
            .is_some_and(|session| !session.running())
        {
            self.channel_test = None;
            self.runtime_notice = Some(t("sound-notice-test-done"));
            self.structure_dirty = true;
        }
        if let Some(session) = &mut self.mic_session {
            let level = session.level();
            if (level - self.mic_level).abs() >= 0.02 {
                self.mic_level = level;
                self.structure_dirty = true;
            }
            if !session.running() {
                self.mic_session = None;
                self.runtime_notice = Some(t("sound-notice-mic-done"));
                self.structure_dirty = true;
            }
        }
        // Re-read the PipeWire graph on the hotplug throttle so a device
        // plugged in or an app that just started playing appears live.
        self.poll_hotplug();
    }

    /// One-time seed of every CRDT key this pane owns, from the initial
    /// backend/prefs snapshot taken at construction.
    fn seed_state(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        let _ = state.put(
            root,
            VOLUME_KEY,
            automerge::ScalarValue::F64(f64::from(self.initial_volume)),
        );
        let _ = state.put(
            root,
            MUTED_KEY,
            automerge::ScalarValue::Boolean(self.last_muted),
        );
        let _ = state.put(
            root,
            BALANCE_KEY,
            automerge::ScalarValue::F64(f64::from(self.initial_prefs.balance.clamp(0.0, 1.0))),
        );
        let _ = state.put(root, MONO_KEY, self.initial_prefs.mono_output);
        let _ = state.put(
            root,
            ECHO_CANCELLATION_KEY,
            self.initial_prefs.echo_cancellation,
        );
        self.seed_system_sounds(state, root);
        for key in [ADVANCED_KEY, TEST_LEFT_KEY, TEST_RIGHT_KEY, MIC_TEST_KEY] {
            let _ = state.put(root, key, false);
        }
        let _ = state.put(
            root,
            INPUT_VOLUME_KEY,
            automerge::ScalarValue::F64(f64::from(self.initial_input)),
        );
        // Seed the device pickers with the current defaults so they rest
        // on the active device.
        if let Some(name) = self.devices.default_sink() {
            let _ = state.put(root, OUTPUT_DEVICE_KEY, name);
        }
        if let Some(name) = self.devices.default_source() {
            let _ = state.put(root, INPUT_DEVICE_KEY, name);
        }
        for hw in &self.graph.hardware {
            if let Some(active) = hw
                .active_profile
                .and_then(|i| hw.profiles.iter().find(|p| p.index == i))
            {
                let _ = state.put(root, device_profile_key(hw.id), active.description.clone());
                self.last_device_profile
                    .insert(hw.id, active.description.clone());
            }
        }
        self.needs_seed = false;
    }

    /// Apply the output volume, input (mic) volume and mute toggles from the
    /// CRDT to the dispatcher/`InputControl`, each guarded so a value at rest
    /// doesn't re-fire.
    fn apply_volume_controls(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(new) = read_f64(state, root, VOLUME_KEY) {
            let new = new as f32;
            if (new - self.last_volume).abs() > 0.001 {
                let operation = crate::actions::AudioOperation::SetOutputVolume {
                    value: f64::from(new),
                };
                if !self.operation_is_running(&operation) {
                    let _ = state.put(root, VOLUME_KEY, f64::from(self.last_volume));
                } else {
                    match self.dispatcher.set_volume(new.clamp(0.0, 1.0) * 100.0) {
                        Ok(()) => {
                            self.last_volume = new;
                            self.finish_local_operation(
                                operation,
                                Ok(t_args(
                                    "sound-notice-out-vol",
                                    &[("percent", i64::from((new * 100.0).round() as u32).into())],
                                )),
                            );
                        }
                        Err(e) => {
                            let _ = state.put(root, VOLUME_KEY, f64::from(self.last_volume));
                            self.runtime_notice = Some(t_args(
                                "sound-notice-vol-err",
                                &[("error", e.clone().into())],
                            ));
                            self.structure_dirty = true;
                            self.finish_local_operation(
                                operation,
                                Err(t_args(
                                    "sound-notice-out-vol-err",
                                    &[("error", e.clone().into())],
                                )),
                            );
                            warn!(target: "tilo::settings", op = "set_volume", error = %e, "audio pane dispatch failed");
                        }
                    }
                }
            }
        }
        if let Some(new) = read_f64(state, root, INPUT_VOLUME_KEY) {
            let new = new as f32;
            if (new - self.last_input).abs() > 0.001 {
                let operation = crate::actions::AudioOperation::SetInputVolume {
                    value: f64::from(new),
                };
                if !self.operation_is_running(&operation) {
                    let _ = state.put(root, INPUT_VOLUME_KEY, f64::from(self.last_input));
                } else {
                    match self.input.set_input_volume(new.clamp(0.0, 1.0) * 100.0) {
                        Ok(()) => {
                            self.last_input = new;
                            self.finish_local_operation(
                                operation,
                                Ok(t_args(
                                    "sound-notice-in-vol",
                                    &[("percent", i64::from((new * 100.0).round() as u32).into())],
                                )),
                            );
                        }
                        Err(e) => {
                            let _ = state.put(root, INPUT_VOLUME_KEY, f64::from(self.last_input));
                            self.runtime_notice = Some(t_args(
                                "sound-notice-in-vol-err",
                                &[("error", e.clone().into())],
                            ));
                            self.structure_dirty = true;
                            self.finish_local_operation(
                                operation,
                                Err(t_args(
                                    "sound-notice-in-vol-err",
                                    &[("error", e.clone().into())],
                                )),
                            );
                            warn!(target: "tilo::settings", op = "set_input_volume", error = %e, "audio pane dispatch failed");
                        }
                    }
                }
            }
        }
        if let Some(b) = read_bool(state, root, MUTED_KEY)
            && b != self.last_muted
        {
            let operation = crate::actions::AudioOperation::SetOutputMuted { muted: b };
            if !self.operation_is_running(&operation) {
                let _ = state.put(root, MUTED_KEY, self.last_muted);
            } else {
                match self.dispatcher.set_mute(b) {
                    Ok(()) => {
                        self.last_muted = b;
                        self.finish_local_operation(
                            operation,
                            Ok(if b {
                                t("sound-notice-muted")
                            } else {
                                t("sound-notice-unmuted")
                            }),
                        );
                    }
                    Err(e) => {
                        let _ = state.put(root, MUTED_KEY, self.last_muted);
                        self.runtime_notice = Some(t_args(
                            "sound-notice-mute-err",
                            &[("error", e.clone().into())],
                        ));
                        self.structure_dirty = true;
                        self.finish_local_operation(
                            operation,
                            Err(t_args(
                                "sound-notice-mute-err",
                                &[("error", e.clone().into())],
                            )),
                        );
                        warn!(target: "tilo::settings", op = "set_mute", error = %e, "audio pane dispatch failed");
                    }
                }
            }
        }
    }

    /// Apply the balance slider and mono toggle from the CRDT, persisting the
    /// preference (never touching the compositor's live session transform).
    fn apply_balance_and_mono(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(value) = read_f64(state, root, BALANCE_KEY).map(|v| v as f32)
            && (value - self.balance).abs() > 0.001
        {
            let requested = value.clamp(0.0, 1.0);
            let operation = crate::actions::AudioOperation::SetBalance {
                value: f64::from(requested),
            };
            if !self.operation_is_running(&operation) {
                let _ = state.put(root, BALANCE_KEY, f64::from(self.balance));
            } else {
                let result = if self.persist_prefs {
                    Self::persist_audio(|prefs| prefs.balance = requested)
                } else {
                    Ok(())
                };
                match result {
                    Ok(()) => {
                        self.balance = requested;
                        self.finish_local_operation(
                            operation,
                            Ok(t_args(
                                "sound-notice-balance",
                                &[(
                                    "percent",
                                    i64::from((requested * 100.0).round() as u32).into(),
                                )],
                            )),
                        );
                    }
                    Err(error) => {
                        let _ = state.put(root, BALANCE_KEY, f64::from(self.balance));
                        self.finish_local_operation(operation, Err(error));
                    }
                }
            }
        }
        if let Some(enabled) = read_bool(state, root, MONO_KEY)
            && enabled != self.mono_enabled
        {
            let operation = crate::actions::AudioOperation::SetMono { enabled };
            if !self.operation_is_running(&operation) {
                let _ = state.put(root, MONO_KEY, self.mono_enabled);
                return;
            }
            let result = if self.persist_prefs {
                Self::persist_audio(|prefs| prefs.mono_output = enabled)
            } else {
                Ok(())
            };
            if let Err(error) = result {
                let _ = state.put(root, MONO_KEY, self.mono_enabled);
                self.finish_local_operation(operation, Err(error));
                return;
            }
            self.mono_enabled = enabled;
            self.finish_local_operation(
                operation,
                Ok(if enabled {
                    t("sound-notice-mono-on")
                } else {
                    t("sound-notice-mono-off")
                }),
            );
            self.runtime_notice = Some(if enabled {
                t("sound-notice-mono-active")
            } else {
                t("sound-notice-mono-restored")
            });
            self.structure_dirty = true;
        }
    }

    /// The CRDT mirror of [`Self::system_sounds`], written once at seed time.
    fn seed_system_sounds(&self, state: &mut AutoCommit, root: &ObjId) {
        let prefs = &self.system_sounds;
        let _ = state.put(root, SYSTEM_SOUNDS_KEY, prefs.enabled);
        let _ = state.put(
            root,
            SYSTEM_SOUND_VOLUME_KEY,
            automerge::ScalarValue::F64(prefs.volume),
        );
        if let Some(theme) = self
            .sound_themes
            .iter()
            .find(|theme| theme.id == prefs.theme_name())
        {
            let _ = state.put(root, SOUND_THEME_KEY, theme.name.as_str());
        }
        for event in SystemSoundEvent::ALL {
            let _ = state.put(root, event_sound_key(event), prefs.events.get(event));
        }
    }

    /// The "System sounds" card: the master switch, then volume, theme and
    /// one switch per event while it is on.
    fn build_system_sounds_card(&self) -> crate::widgets::El {
        if !self.event_sounds_available {
            return card(
                &t("sound-system"),
                rows(vec![caption(&t("sound-system-missing"))]),
            );
        }
        let mut sound_rows = vec![setting_row(
            &t("sound-system"),
            Box::new(
                Toggle::new("settings.audio.system_sounds", SYSTEM_SOUNDS_KEY)
                    .with_id("audio.system_sounds")
                    .on_action(
                        crate::actions::audio_bool_binding("system-sounds")
                            .expect("system sounds are part of the static resource contract"),
                    ),
            ),
        )];
        if !self.system_sounds.enabled {
            return card(&t("sound-system"), rows(sound_rows));
        }
        sound_rows.push(setting_row(
            &t("sound-volume"),
            Box::new(
                Slider::new(
                    "settings.audio.system_sound_volume",
                    SYSTEM_SOUND_VOLUME_KEY,
                )
                .with_id("audio.system_sound_volume")
                .on_action(
                    crate::actions::audio_number_binding("system-sound-volume")
                        .expect("system sound volume is part of the static resource contract"),
                ),
            ),
        ));
        let themes: Vec<(String, String)> = self
            .sound_themes
            .iter()
            .map(|theme| (theme.name.clone(), theme.id.clone()))
            .collect();
        if themes.len() >= 2
            && let Some(binding) = crate::actions::audio_sound_theme_binding(themes.clone())
        {
            let names = themes.into_iter().map(|(name, _)| name).collect();
            sound_rows.push(setting_row(
                &t("sound-theme"),
                Box::new(
                    Dropdown::new(names, SOUND_THEME_KEY)
                        .with_id("audio.sound_theme")
                        .on_action(binding),
                ),
            ));
        }
        for event in SystemSoundEvent::ALL {
            let action = crate::actions::event_sound_action_id(event);
            sound_rows.push(setting_row(
                &event_shown(event),
                Box::new(
                    Toggle::new(
                        &format!("settings.audio.event_sound.{}", event.key()),
                        &event_sound_key(event),
                    )
                    .with_id(&format!("audio.event_sound.{}", event.key()))
                    .on_action(
                        crate::actions::audio_bool_binding(&action)
                            .expect("every system sound event is part of the resource contract"),
                    ),
                ),
            ));
        }
        sound_rows.push(caption(&t("sound-system-caption")));
        card(&t("sound-system"), rows(sound_rows))
    }

    /// Persist "System sounds" changes. The compositor reads the saved
    /// preference whenever an event would make a sound.
    fn apply_system_sounds(&mut self, state: &mut AutoCommit, root: &ObjId) {
        use crate::actions::AudioOperation as Op;
        let mut next = self.system_sounds.clone();
        let mut operations = Vec::new();
        if let Some(enabled) =
            read_bool(state, root, SYSTEM_SOUNDS_KEY).filter(|enabled| *enabled != next.enabled)
        {
            next.enabled = enabled;
            operations.push((
                Op::SetSystemSounds { enabled },
                SYSTEM_SOUNDS_KEY.to_string(),
            ));
        }
        if let Some(value) = read_f64(state, root, SYSTEM_SOUND_VOLUME_KEY)
            .map(|value| value.clamp(0.0, 1.0))
            .filter(|value| (value - next.volume).abs() > 0.001)
        {
            next.volume = value;
            operations.push((
                Op::SetSystemSoundVolume { value },
                SYSTEM_SOUND_VOLUME_KEY.to_string(),
            ));
        }
        if let Some(theme) = read_str_or_text(state, root, SOUND_THEME_KEY)
            .and_then(|name| self.sound_themes.iter().find(|theme| theme.name == name))
            .filter(|theme| theme.id != next.theme_name())
        {
            next.theme = Some(theme.id.clone());
            operations.push((
                Op::SetSoundTheme {
                    theme: theme.id.clone(),
                },
                SOUND_THEME_KEY.to_string(),
            ));
        }
        for event in SystemSoundEvent::ALL {
            let key = event_sound_key(event);
            if let Some(enabled) = read_bool(state, root, key.as_str())
                .filter(|enabled| *enabled != next.events.get(event))
            {
                next.events.set(event, enabled);
                operations.push((
                    Op::SetEventSound {
                        event: event.key().to_string(),
                        enabled,
                    },
                    key,
                ));
            }
        }
        if operations.is_empty() {
            return;
        }
        let next = next.sanitized();
        let allowed = operations
            .iter()
            .all(|(operation, _)| self.operation_is_running(operation));
        let result = if !allowed {
            Err(None)
        } else if self.persist_prefs {
            let saved = next.clone();
            Self::persist_audio(move |prefs| prefs.system_sounds = saved).map_err(Some)
        } else {
            Ok(())
        };
        match result {
            Ok(()) => {
                let enabled_changed = next.enabled != self.system_sounds.enabled;
                self.system_sounds = next;
                for (operation, _) in operations {
                    self.finish_local_operation(operation, Ok(t("sound-notice-sounds-ok")));
                }
                if enabled_changed {
                    self.structure_dirty = true;
                }
            }
            Err(error) => {
                // Put the controls back to what is actually saved.
                self.seed_system_sounds(state, root);
                if let Some(error) = error {
                    for (operation, _) in operations {
                        self.finish_local_operation(operation, Err(error.clone()));
                    }
                }
            }
        }
    }

    /// Persist the "Reduce echo in calls" toggle. The compositor session
    /// starts or stops the echo-cancelled microphone from the saved value.
    fn apply_echo_cancellation(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(enabled) = read_bool(state, root, ECHO_CANCELLATION_KEY)
            .filter(|enabled| *enabled != self.echo_cancellation)
        else {
            return;
        };
        let operation = crate::actions::AudioOperation::SetEchoCancellation { enabled };
        if !self.operation_is_running(&operation) {
            let _ = state.put(root, ECHO_CANCELLATION_KEY, self.echo_cancellation);
            return;
        }
        let result = if self.persist_prefs {
            Self::persist_audio(|prefs| prefs.echo_cancellation = enabled)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            let _ = state.put(root, ECHO_CANCELLATION_KEY, self.echo_cancellation);
            self.finish_local_operation(operation, Err(error));
            return;
        }
        self.echo_cancellation = enabled;
        self.finish_local_operation(
            operation,
            Ok(if enabled {
                t("sound-notice-echo-on")
            } else {
                t("sound-notice-echo-off")
            }),
        );
    }

    /// Handle the advanced-panel toggle and the one-shot speaker/microphone
    /// test buttons.
    fn apply_diagnostics_requests(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, ADVANCED_KEY) == Some(true) {
            let _ = state.put(root, ADVANCED_KEY, false);
            let operation = crate::actions::AudioOperation::ToggleDiagnostics;
            if !self.operation_is_running(&operation) {
                return;
            }
            self.advanced = !self.advanced;
            if !self.advanced {
                self.channel_test = None;
            }
            self.structure_dirty = true;
            self.finish_local_operation(
                operation,
                Ok(if self.advanced {
                    t("sound-notice-diag-on")
                } else {
                    t("sound-notice-diag-off")
                }),
            );
        }
        for (key, left) in [(TEST_LEFT_KEY, true), (TEST_RIGHT_KEY, false)] {
            if read_bool(state, root, key) != Some(true) {
                continue;
            }
            let _ = state.put(root, key, false);
            let operation = crate::actions::AudioOperation::TestChannel { left };
            if !self.operation_is_running(&operation) {
                continue;
            }
            self.channel_test = None;
            let result = self
                .graph
                .default_sink()
                .filter(|sink| sink.channels == 2)
                .ok_or_else(|| t("sound-notice-test-need"))
                .and_then(|sink| self.runtime.start_channel_test(&sink.name, left));
            match result {
                Ok(session) => {
                    self.channel_test = Some(session);
                    let channel = if left {
                        t("sound-test-left")
                    } else {
                        t("sound-test-right")
                    };
                    self.runtime_notice = Some(t_args(
                        "sound-notice-playing",
                        &[("channel", channel.as_str().into())],
                    ));
                    self.finish_local_operation(
                        operation,
                        Ok(t_args(
                            "sound-notice-test-started",
                            &[("channel", channel.into())],
                        )),
                    );
                }
                Err(error) => {
                    let channel = if left {
                        t("sound-test-left")
                    } else {
                        t("sound-test-right")
                    };
                    let notice = t_args(
                        "sound-notice-test-err",
                        &[
                            ("channel", channel.as_str().into()),
                            ("error", error.clone().into()),
                        ],
                    );
                    self.runtime_notice = Some(notice.clone());
                    self.finish_local_operation(operation, Err(notice));
                }
            }
            self.structure_dirty = true;
        }
        if read_bool(state, root, MIC_TEST_KEY) == Some(true) {
            let _ = state.put(root, MIC_TEST_KEY, false);
            let operation = crate::actions::AudioOperation::ToggleMicrophoneMeter;
            if !self.operation_is_running(&operation) {
                return;
            }
            if self.mic_session.is_some() {
                self.mic_session = None;
                self.runtime_notice = Some(t("sound-notice-mic-stopped"));
                self.finish_local_operation(operation, Ok(t("sound-notice-mic-stopped")));
            } else {
                let result = self
                    .graph
                    .default_source()
                    .ok_or_else(|| t("sound-notice-mic-none"))
                    .and_then(|source| {
                        self.runtime
                            .start_microphone_meter(&source.name, Duration::from_secs(8))
                    });
                match result {
                    Ok(session) => {
                        self.mic_session = Some(session);
                        self.mic_level = 0.0;
                        self.runtime_notice = Some(t("sound-notice-mic-started"));
                        self.finish_local_operation(operation, Ok(t("sound-notice-mic-started")));
                    }
                    Err(error) => {
                        self.runtime_notice = Some(t_args(
                            "sound-notice-mic-err",
                            &[("error", error.clone().into())],
                        ));
                        self.finish_local_operation(
                            operation,
                            Err(t_args(
                                "sound-notice-mic-err",
                                &[("error", error.into())],
                            )),
                        );
                    }
                }
            }
            self.structure_dirty = true;
        }
    }

    /// Apply an output/input device picker selection, dispatching
    /// `wpctl set-default` and persisting the preference.
    fn apply_device_selection(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Output-device (sink) selection → wpctl set-default, guarded so a
        // selection at rest doesn't re-fire.
        if let Some(name) = read_str_or_text(state, root, OUTPUT_DEVICE_KEY)
            && self.last_sink.as_deref() != Some(name.as_str())
            && let Some(id) = self.devices.sink_id(&name)
        {
            let operation = crate::actions::AudioOperation::SelectOutput { device_id: id };
            if !self.operation_is_running(&operation) {
                if let Some(previous) = self.last_sink.as_deref() {
                    let _ = state.put(root, OUTPUT_DEVICE_KEY, previous);
                }
                return;
            }
            match self.device.set_default(id) {
                Ok(()) => {
                    if self.persist_prefs {
                        let nm = name.clone();
                        let _ = Self::persist_audio(|a| a.output_device = Some(nm));
                    }
                    self.last_sink = Some(name);
                    self.finish_local_operation(operation, Ok(t("sound-notice-out-device")));
                }
                Err(e) => {
                    if let Some(previous) = self.last_sink.as_deref() {
                        let _ = state.put(root, OUTPUT_DEVICE_KEY, previous);
                    }
                    self.runtime_notice = Some(t_args(
                        "sound-notice-device-err",
                        &[("error", e.clone().into())],
                    ));
                    self.structure_dirty = true;
                    self.finish_local_operation(
                        operation,
                        Err(t_args(
                            "sound-notice-device-err",
                            &[("error", e.clone().into())],
                        )),
                    );
                    warn!(target: "tilo::settings", op = "set_default_sink", error = %e, "audio pane device switch failed");
                }
            }
        }
        // Input-device (source) selection → wpctl set-default.
        if let Some(name) = read_str_or_text(state, root, INPUT_DEVICE_KEY)
            && self.last_source.as_deref() != Some(name.as_str())
            && let Some(id) = self.devices.source_id(&name)
        {
            let operation = crate::actions::AudioOperation::SelectInput { device_id: id };
            if !self.operation_is_running(&operation) {
                if let Some(previous) = self.last_source.as_deref() {
                    let _ = state.put(root, INPUT_DEVICE_KEY, previous);
                }
                return;
            }
            match self.device.set_default(id) {
                Ok(()) => {
                    if self.persist_prefs {
                        let nm = name.clone();
                        let _ = Self::persist_audio(|a| a.input_device = Some(nm));
                    }
                    self.last_source = Some(name);
                    self.finish_local_operation(operation, Ok(t("sound-notice-in-device")));
                }
                Err(e) => {
                    if let Some(previous) = self.last_source.as_deref() {
                        let _ = state.put(root, INPUT_DEVICE_KEY, previous);
                    }
                    self.runtime_notice = Some(t_args(
                        "sound-notice-in-device-err",
                        &[("error", e.clone().into())],
                    ));
                    self.structure_dirty = true;
                    self.finish_local_operation(
                        operation,
                        Err(t_args(
                            "sound-notice-in-device-err",
                            &[("error", e.clone().into())],
                        )),
                    );
                    warn!(target: "tilo::settings", op = "set_default_source", error = %e, "audio pane device switch failed");
                }
            }
        }
    }

    /// Seed and dispatch each per-application stream's route/volume/mute,
    /// then apply any hardware device-profile picker change.
    fn apply_streams_and_profiles(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Per-application streams: seed each stream's route/volume/mute keys
        // once, then apply any change through the graph control. Cloned out of
        // the graph so the seed/dispatch helpers can borrow `self` mutably.
        let streams: Vec<AudioStream> = self
            .graph
            .playback_streams
            .iter()
            .chain(self.graph.record_streams.iter())
            .cloned()
            .collect();
        for s in &streams {
            self.seed_stream(state, root, s);
        }
        for s in &streams {
            self.dispatch_stream(state, root, s);
        }
        for hw in &self.graph.hardware {
            let Some(choice) = read_str_or_text(state, root, device_profile_key(hw.id)) else {
                continue;
            };
            if self.last_device_profile.get(&hw.id) == Some(&choice) {
                continue;
            }
            let Some(profile) = hw
                .profiles
                .iter()
                .find(|p| p.available && p.description == choice)
            else {
                continue;
            };
            let operation = crate::actions::AudioOperation::SetDeviceProfile {
                device_id: hw.id,
                profile_index: profile.index,
            };
            if !self.operation_is_running(&operation) {
                if let Some(previous) = self.last_device_profile.get(&hw.id) {
                    let _ = state.put(root, device_profile_key(hw.id), previous.as_str());
                }
                continue;
            }
            match self.gctl.set_device_profile(hw.id, profile.index) {
                Ok(()) => {
                    self.last_device_profile.insert(hw.id, choice);
                    self.finish_local_operation(operation, Ok(t("sound-notice-profile")));
                }
                Err(e) => {
                    if let Some(previous) = self.last_device_profile.get(&hw.id) {
                        let _ = state.put(root, device_profile_key(hw.id), previous.as_str());
                    }
                    self.runtime_notice = Some(t_args(
                        "sound-notice-profile-err",
                        &[("error", e.clone().into())],
                    ));
                    self.structure_dirty = true;
                    self.finish_local_operation(
                        operation,
                        Err(t_args(
                            "sound-notice-profile-err",
                            &[("error", e.clone().into())],
                        )),
                    );
                    warn!(target: "tilo::settings", op = "set_device_profile", device = hw.id, error = %e, "audio profile change failed");
                }
            }
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AudioPane::new())
}

impl Widget for AudioPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        self.tick_sessions_and_hotplug();
        self.seed_state(&mut state, &root);
        if let Some(crate::actions::LocalPaneCommand::Audio(operation)) = self.command_state.get() {
            self.command_state.set(None);
            self.execute_local_command(&mut state, operation);
        }
        if self.has_pending_operation() {
            self.apply_volume_controls(&mut state, &root);
            self.apply_balance_and_mono(&mut state, &root);
            self.apply_echo_cancellation(&mut state, &root);
            self.apply_system_sounds(&mut state, &root);
            self.apply_diagnostics_requests(&mut state, &root);
            self.apply_device_selection(&mut state, &root);
            self.apply_streams_and_profiles(&mut state, &root);
        }
        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "AudioPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|child| child.take_structure_dirty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records the microphone gains the input slider forwards, so the test
    /// can assert the source volume reached the backend.
    #[derive(Default)]
    struct RecordingInput {
        calls: Mutex<Vec<f32>>,
    }

    struct FakeSession {
        drops: Arc<AtomicUsize>,
    }
    impl BoundedSession for FakeSession {
        fn running(&mut self) -> bool {
            true
        }
        fn stop(&mut self) {}
    }
    impl Drop for FakeSession {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct FakeMeter {
        session: FakeSession,
    }
    impl BoundedSession for FakeMeter {
        fn running(&mut self) -> bool {
            self.session.running()
        }
        fn stop(&mut self) {
            self.session.stop();
        }
    }
    impl MicrophoneMeterSession for FakeMeter {
        fn level(&mut self) -> f32 {
            0.42
        }
        fn remaining(&self) -> Duration {
            Duration::from_secs(4)
        }
    }
    struct RecordingRuntime {
        calls: Mutex<Vec<String>>,
        drops: Arc<AtomicUsize>,
    }
    impl AudioRuntimeControl for RecordingRuntime {
        fn capabilities(&self) -> RuntimeCapabilities {
            RuntimeCapabilities {
                balance: true,
                mono_output: true,
                channel_test: true,
                microphone_meter: true,
                echo_cancellation: true,
            }
        }
        fn set_balance(&self, sink: &str, base: f32, balance: f32) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("balance({sink},{base:.2},{balance:.2})"));
            Ok(())
        }
        fn start_mono(&self, sink: &str) -> Result<Box<dyn BoundedSession>, String> {
            self.calls.lock().unwrap().push(format!("mono({sink})"));
            Ok(Box::new(FakeSession {
                drops: self.drops.clone(),
            }))
        }
        fn start_channel_test(
            &self,
            sink: &str,
            left: bool,
        ) -> Result<Box<dyn BoundedSession>, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("test({sink},{left})"));
            Ok(Box::new(FakeSession {
                drops: self.drops.clone(),
            }))
        }
        fn start_microphone_meter(
            &self,
            source: &str,
            _: Duration,
        ) -> Result<Box<dyn MicrophoneMeterSession>, String> {
            self.calls.lock().unwrap().push(format!("meter({source})"));
            Ok(Box::new(FakeMeter {
                session: FakeSession {
                    drops: self.drops.clone(),
                },
            }))
        }
    }
    impl InputControl for RecordingInput {
        fn set_input_volume(&self, pct: f32) -> Result<(), String> {
            self.calls.lock().unwrap().push(pct);
            Ok(())
        }
    }

    /// Volume, mute and input sliders dispatch; replicated latches are inert.
    #[test]
    fn audio_device_controls_dispatch_only_local_intent() {
        // Case `volume_slider_invokes_set_volume_on_drag`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, VOLUME_KEY, automerge::ScalarValue::F64(0.30));
            pane.sync(&mut doc, &root);
            let calls = rec.calls();
            assert_eq!(calls.len(), 1, "expected one dispatch, got {calls:?}");
            assert!(calls[0].starts_with("set_volume("));
            assert!(calls[0].contains("30."), "expected 30 in {:?}", calls[0]);
        }

        // Case `mute_toggle_sets_the_requested_state`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, MUTED_KEY, automerge::ScalarValue::Boolean(true));
            pane.sync(&mut doc, &root);
            let calls = rec.calls();
            assert_eq!(calls, vec!["set_mute(true)"]);
        }

        // Case `replicated_audio_latches_are_inert_without_a_local_token`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
            let commands = ui::LocalState::new(None);
            let mut pane =
                AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                    .with_command_state(&commands)
                    .with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let initial_heads = doc.get_heads();
            pane.sync(&mut doc, &root);
            assert_eq!(doc.get_heads(), initial_heads);
            let _ = doc.put(&root, VOLUME_KEY, 0.9);
            let _ = doc.put(&root, MUTED_KEY, true);
            pane.sync(&mut doc, &root);
            assert!(rec.calls().is_empty());
            assert!(!pane.last_muted);
        }

        // Case `input_slider_invokes_input_control_on_drag`.
        // The input-level slider forwards the new gain to the microphone
        // `InputControl` exactly once per change, and not while it rests.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let input = Arc::new(RecordingInput::default());
            let mut pane = AudioPane::with_controls(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                input.clone(),
                0.5,
                0.75,
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(
                input.calls.lock().unwrap().is_empty(),
                "seed must not dispatch"
            );
            // User drags the input slider to 40%.
            let _ = doc.put(&root, INPUT_VOLUME_KEY, automerge::ScalarValue::F64(0.40));
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root); // idempotent while at rest
            let calls = input.calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 1, "one input dispatch per change: {calls:?}");
            assert!(
                (calls[0] - 40.0).abs() < 0.5,
                "expected ~40%, got {}",
                calls[0]
            );
        }
    }

    /// Records the device ids a `set_default` call switches to, so the picker
    /// test can assert the right node id reached the backend.
    #[derive(Default)]
    struct RecordingDevice {
        calls: Mutex<Vec<u32>>,
    }
    impl DeviceControl for RecordingDevice {
        fn set_default(&self, id: u32) -> Result<(), String> {
            self.calls.lock().unwrap().push(id);
            Ok(())
        }
    }

    fn sample_status() -> &'static str {
        "\
PipeWire 'pipewire-0' [1.0.5, ada@host, cookie:1]
 └─ Clients:
        32. WirePlumber

Audio
 ├─ Devices:
 │      45. Built-in Audio               [alsa]
 │
 ├─ Sinks:
 │  *   51. Built-in Audio Analog Stereo [vol: 0.65]
 │      52. HDMI / DisplayPort           [vol: 1.00]
 │
 ├─ Sources:
 │  *   53. Built-in Audio Analog Stereo [vol: 0.80]
 │      54. USB Microphone               [vol: 0.90]
 │
 └─ Streams:

Video
 ├─ Devices:
 │      60. Integrated Camera            [v4l2]
"
    }

    /// wpctl parsing, default sink/source switching, profiles and the persisted default.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn audio_devices_parse_and_default_selection_switches_once() {
        // Case `parse_wpctl_status_extracts_sinks_sources_and_default`.
        {
            let d = parse_wpctl_status(sample_status());
            assert_eq!(d.sinks.len(), 2, "sinks: {:?}", d.sinks);
            assert_eq!(d.sources.len(), 2, "sources: {:?}", d.sources);
            assert_eq!(d.sinks[0].id, 51);
            assert_eq!(d.sinks[0].name, "Built-in Audio Analog Stereo");
            assert!(d.sinks[0].default, "the * marks the default sink");
            assert_eq!(d.sinks[1].name, "HDMI / DisplayPort");
            assert!(!d.sinks[1].default);
            assert_eq!(d.default_sink(), Some("Built-in Audio Analog Stereo"));
            // The camera in the Video section must not leak into audio devices.
            assert!(!d.sources.iter().any(|s| s.name.contains("Camera")));
            assert_eq!(d.sources[1].id, 54);
            assert_eq!(d.sources[1].name, "USB Microphone");
        }

        // Case `parse_wpctl_status_empty_when_no_audio`.
        {
            assert_eq!(parse_wpctl_status(""), AudioDevices::default());
        }

        // Case `selecting_output_device_switches_default_once`.
        // Choosing a different output device runs `set-default` with that sink's
        // node id exactly once, and not again while it rests.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let dev = Arc::new(RecordingDevice::default());
            let devices = parse_wpctl_status(sample_status());
            let mut pane = AudioPane::with_controls(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                Arc::new(RealInput),
                0.5,
                0.75,
            )
            .with_devices(devices, dev.clone() as Arc<dyn DeviceControl>);
            assert_eq!(pane.devices().sinks.len(), 2, "picker has both sinks");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(dev.calls.lock().unwrap().is_empty(), "seed must not switch");
            // Pick the HDMI sink (node 52).
            let _ = doc.put(&root, OUTPUT_DEVICE_KEY, "HDMI / DisplayPort");
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root); // idempotent at rest
            assert_eq!(dev.calls.lock().unwrap().as_slice(), [52]);
        }

        // Case `selecting_input_device_switches_source_default`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let dev = Arc::new(RecordingDevice::default());
            let devices = parse_wpctl_status(sample_status());
            let mut pane = AudioPane::with_controls(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                Arc::new(RealInput),
                0.5,
                0.75,
            )
            .with_devices(devices, dev.clone() as Arc<dyn DeviceControl>);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, INPUT_DEVICE_KEY, "USB Microphone");
            pane.sync(&mut doc, &root);
            assert_eq!(dev.calls.lock().unwrap().as_slice(), [54]);
        }

        // Case `selecting_reported_profile_dispatches_real_pipewire_index`.
        {
            let rec = Arc::new(RecordingGraph::default());
            let mut graph = graph_two_sinks_one_stream();
            graph.hardware.push(AudioHardwareDevice {
                id: 7,
                description: "Headset".into(),
                active_profile: Some(1),
                profiles: vec![
                    DeviceProfile {
                        index: 1,
                        name: "a2dp".into(),
                        description: "High Fidelity".into(),
                        available: true,
                    },
                    DeviceProfile {
                        index: 2,
                        name: "hfp".into(),
                        description: "Headset microphone".into(),
                        available: true,
                    },
                ],
            });
            let disp = Arc::new(RecordingDispatcher::default());
            let mut pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.5, 0.75)
                .with_graph(graph, rec.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, device_profile_key(7), "Headset microphone");
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            assert_eq!(rec.calls(), vec!["profile(7,2)"]);
        }

        // Case `choosing_a_default_sink_persists_the_preference`.
        {
            // Redirect the config dir so this never touches the real settings.json.
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let rec = Arc::new(RecordingDispatcher::default());
            let dev = Arc::new(RecordingDevice::default());
            let devices = parse_wpctl_status(sample_status());
            let mut pane = AudioPane::with_controls(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                Arc::new(RealInput),
                0.5,
                0.75,
            )
            .with_devices(devices, dev as Arc<dyn DeviceControl>);
            // Enable the real persist path for this test (the test constructors
            // disable it so the suite never writes real prefs).
            pane.persist_prefs = true;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Choose the HDMI output as the default sink.
            let _ = doc.put(&root, OUTPUT_DEVICE_KEY, "HDMI / DisplayPort");
            pane.sync(&mut doc, &root);
            // The choice survives to the shared prefs store.
            let saved = SystemPrefs::load().audio.output_device;
            assert_eq!(
                saved.as_deref(),
                Some("HDMI / DisplayPort"),
                "default sink preference persisted"
            );
        }
    }

    // --- Per-application routing -------------------------------------------

    use media::audio::{
        AudioDevice as GDevice, AudioGraph as G, AudioHardwareDevice, AudioStream as GStream,
        DeviceProfile, GraphControl,
    };

    /// Records every graph edit the pane dispatches, so a routing/volume/mute
    /// change can be asserted without touching `pw-metadata` / `wpctl`.
    #[derive(Default)]
    struct RecordingGraph {
        calls: Mutex<Vec<String>>,
    }
    impl RecordingGraph {
        fn push(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl GraphControl for RecordingGraph {
        fn set_default_sink(&self, id: u32) -> Result<(), String> {
            self.push(format!("default_sink({id})"));
            Ok(())
        }
        fn set_default_source(&self, id: u32) -> Result<(), String> {
            self.push(format!("default_source({id})"));
            Ok(())
        }
        fn route_stream(&self, stream: u32, serial: u32) -> Result<(), String> {
            self.push(format!("route({stream},{serial})"));
            Ok(())
        }
        fn unroute_stream(&self, stream: u32) -> Result<(), String> {
            self.push(format!("unroute({stream})"));
            Ok(())
        }
        fn set_node_volume(&self, id: u32, v: f32) -> Result<(), String> {
            self.push(format!("volume({id},{v:.2})"));
            Ok(())
        }
        fn set_node_mute(&self, id: u32, m: bool) -> Result<(), String> {
            self.push(format!("mute({id},{m})"));
            Ok(())
        }
        fn set_device_profile(&self, id: u32, profile: u32) -> Result<(), String> {
            self.push(format!("profile({id},{profile})"));
            Ok(())
        }
    }

    /// Two sinks (the default "Speakers", and "Headphones" as a routing target)
    /// and one playback stream (node 84) currently following the default.
    fn graph_two_sinks_one_stream() -> G {
        G {
            sinks: vec![
                GDevice {
                    id: 51,
                    serial: 51,
                    name: "alsa_speakers".into(),
                    description: "Speakers".into(),
                    is_default: true,
                    volume: Some(0.8),
                    muted: Some(false),
                    channels: 2,
                    codec: None,
                },
                GDevice {
                    id: 91,
                    serial: 269,
                    name: "alsa_hp".into(),
                    description: "Headphones".into(),
                    is_default: false,
                    volume: Some(1.0),
                    muted: Some(false),
                    channels: 2,
                    codec: None,
                },
            ],
            sources: vec![],
            playback_streams: vec![GStream {
                id: 84,
                serial: 300,
                app: "music".into(),
                media_name: Some("A Song".into()),
                playback: true,
                target_id: Some(51),
                volume: Some(0.5),
                muted: Some(false),
            }],
            record_streams: vec![],
            hardware: vec![],
        }
    }

    fn routing_pane(gctl: Arc<dyn GraphControl>) -> AudioPane {
        let disp = Arc::new(RecordingDispatcher::default());
        AudioPane::with_controls(
            disp as Arc<dyn Dispatcher + Send + Sync>,
            Arc::new(RealInput),
            0.5,
            0.75,
        )
        .with_graph(graph_two_sinks_one_stream(), gctl)
    }

    /// Bounded diagnostics, hotplugged sinks, and balance/mono persistence without owning the transform.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn audio_diagnostics_hotplug_and_transforms_stay_bounded() {
        // Case `diagnostics_are_bounded_while_persistent_transforms_have_no_pane_owner`.
        {
            let drops = Arc::new(AtomicUsize::new(0));
            let runtime = Arc::new(RecordingRuntime {
                calls: Mutex::new(vec![]),
                drops: drops.clone(),
            });
            let mut graph = graph_two_sinks_one_stream();
            graph.sources.push(media::audio::AudioDevice {
                id: 52,
                serial: 52,
                name: "alsa_input.mic".into(),
                description: "Microphone".into(),
                is_default: true,
                volume: Some(0.7),
                muted: Some(false),
                channels: 1,
                codec: None,
            });
            let disp = Arc::new(RecordingDispatcher::default());
            let mut pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.5, 0.75)
                .with_graph(graph, Arc::new(NoopGraphControl))
                .with_runtime(runtime.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, BALANCE_KEY, 0.75);
            let _ = doc.put(&root, MONO_KEY, true);
            let _ = doc.put(&root, TEST_LEFT_KEY, true);
            let _ = doc.put(&root, MIC_TEST_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                runtime.calls.lock().unwrap().as_slice(),
                ["test(alsa_speakers,true)", "meter(alsa_input.mic)",]
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            drop(pane);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                2,
                "only bounded tone and mic diagnostics belong to this pane"
            );
        }

        // Case `hotplug_adds_a_newly_plugged_sink`.
        {
            // Start with only the Speakers sink; the graph the enumerator returns
            // has gained a Headphones sink (a headset was plugged in).
            let mut before = graph_two_sinks_one_stream();
            before.sinks.retain(|d| d.description == "Speakers");
            before.playback_streams.clear();
            let after = {
                let mut g = graph_two_sinks_one_stream();
                g.playback_streams.clear();
                g
            };
            let rec = Arc::new(RecordingGraph::default());
            let disp = Arc::new(RecordingDispatcher::default());
            let mut pane = AudioPane::with_controls(
                disp as Arc<dyn Dispatcher + Send + Sync>,
                Arc::new(RealInput),
                0.5,
                0.75,
            )
            .with_graph(before, rec)
            .with_hotplug(Arc::new(Scripted {
                seq: vec![after],
                idx: Mutex::new(0),
            }));
            assert_eq!(pane.graph().sinks.len(), 1, "starts with one sink");

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root); // re-enumerates → adopts the plugged-in sink

            assert_eq!(
                pane.graph().sinks.len(),
                2,
                "the headset appeared in the graph"
            );
            assert!(
                pane.graph()
                    .sinks
                    .iter()
                    .any(|d| d.description == "Headphones")
            );
            // …and it is selectable in the output-device picker (device view).
            assert_eq!(
                pane.devices().sinks.len(),
                2,
                "the picker gained the headset"
            );
            assert!(pane.devices().sinks.iter().any(|d| d.name == "Headphones"));
        }

        // Case `balance_and_mono_persist_without_settings_owning_the_transform`.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let runtime = Arc::new(RecordingRuntime {
                calls: Mutex::new(Vec::new()),
                drops: Arc::new(AtomicUsize::new(0)),
            });
            let disp = Arc::new(RecordingDispatcher::default());
            let mut pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.5, 0.75)
                .with_graph(graph_two_sinks_one_stream(), Arc::new(NoopGraphControl))
                .with_runtime(runtime.clone());
            pane.persist_prefs = true;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, BALANCE_KEY, 0.8);
            let _ = doc.put(&root, MONO_KEY, true);
            pane.sync(&mut doc, &root);

            let saved = SystemPrefs::load().audio;
            assert!((saved.balance - 0.8).abs() < 0.001);
            assert!(saved.mono_output);
            assert!(
                runtime.calls.lock().unwrap().is_empty(),
                "the compositor session, not Settings, actuates persistent transforms"
            );
        }
    }

    /// Stream seeding rests; routing, unrouting, per-app volume and mute dispatch once.
    #[test]
    fn per_app_streams_route_and_mix_once_each() {
        // Case `stream_seed_rests_without_dispatch`.
        {
            let rec = Arc::new(RecordingGraph::default());
            let mut pane = routing_pane(rec.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            assert!(
                rec.calls().is_empty(),
                "seeding a stream must dispatch nothing: {:?}",
                rec.calls()
            );
            // The stream on the default sink seeds its picker to "Default".
            assert_eq!(
                read_str_or_text(&doc, &root, stream_sink_key(84)).as_deref(),
                Some(FOLLOW_DEFAULT)
            );
        }

        // Case `routing_one_app_to_a_sink_moves_only_that_stream_once`.
        {
            let rec = Arc::new(RecordingGraph::default());
            let mut pane = routing_pane(rec.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Route just the music app to the Headphones sink.
            let _ = doc.put(&root, stream_sink_key(84).as_str(), "Headphones");
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root); // idempotent at rest
            // Headphones' object.serial is 269 — the routing handle, not its node id.
            assert_eq!(rec.calls(), vec!["route(84,269)"]);
        }

        // Case `routing_an_app_back_to_default_unroutes_it`.
        {
            let rec = Arc::new(RecordingGraph::default());
            let mut pane = routing_pane(rec.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Move it off default…
            let _ = doc.put(&root, stream_sink_key(84).as_str(), "Headphones");
            pane.sync(&mut doc, &root);
            // …then back to Default clears the explicit target.
            let _ = doc.put(&root, stream_sink_key(84).as_str(), FOLLOW_DEFAULT);
            pane.sync(&mut doc, &root);
            assert_eq!(rec.calls(), vec!["route(84,269)", "unroute(84)"]);
        }

        // Case `per_app_volume_and_mute_dispatch_once_each`.
        {
            let rec = Arc::new(RecordingGraph::default());
            let mut pane = routing_pane(rec.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(
                &root,
                stream_volume_key(84).as_str(),
                automerge::ScalarValue::F64(0.30),
            );
            let _ = doc.put(
                &root,
                stream_muted_key(84).as_str(),
                automerge::ScalarValue::Boolean(true),
            );
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root); // idempotent at rest
            assert_eq!(rec.calls(), vec!["volume(84,0.30)", "mute(84,true)"]);
        }
    }

    /// A scripted enumerator: each `enumerate()` yields the next graph in the
    /// sequence, holding on the last — so a test can present a "device plugged
    /// in" snapshot on demand.
    struct Scripted {
        seq: Vec<G>,
        idx: Mutex<usize>,
    }
    impl AudioEnumerator for Scripted {
        fn enumerate(&self) -> G {
            let mut i = self.idx.lock().unwrap();
            let g = self.seq[(*i).min(self.seq.len() - 1)].clone();
            if *i < self.seq.len() - 1 {
                *i += 1;
            }
            g
        }
    }

    #[test]
    fn reduce_echo_in_calls_round_trips_through_the_shared_store() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        assert!(
            SystemPrefs::load().audio.echo_cancellation,
            "calls are echo-cancelled until the person turns it off"
        );
        let runtime = Arc::new(RecordingRuntime {
            calls: Mutex::new(Vec::new()),
            drops: Arc::new(AtomicUsize::new(0)),
        });
        let disp = Arc::new(RecordingDispatcher::default());
        let mut pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.5, 0.75)
            .with_graph(graph_two_sinks_one_stream(), Arc::new(NoopGraphControl))
            .with_runtime(runtime.clone());
        pane.persist_prefs = true;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(read_bool(&doc, &root, ECHO_CANCELLATION_KEY), Some(true));

        let _ = doc.put(&root, ECHO_CANCELLATION_KEY, false);
        pane.sync(&mut doc, &root);
        assert!(!SystemPrefs::load().audio.echo_cancellation);

        let _ = doc.put(&root, ECHO_CANCELLATION_KEY, true);
        pane.sync(&mut doc, &root);
        assert!(SystemPrefs::load().audio.echo_cancellation);
        assert!(
            runtime.calls.lock().unwrap().is_empty(),
            "the compositor session, not Settings, owns the echo-cancelled microphone"
        );
    }

    #[test]
    fn system_sound_choices_round_trip_through_the_shared_store() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let disp = Arc::new(RecordingDispatcher::default());
        let mut pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.5, 0.75)
            .with_sound_themes(vec![
                SoundTheme {
                    id: "freedesktop".into(),
                    name: "Default".into(),
                },
                SoundTheme {
                    id: "ocean".into(),
                    name: "Ocean".into(),
                },
            ]);
        pane.persist_prefs = true;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(read_bool(&doc, &root, SYSTEM_SOUNDS_KEY), Some(true));
        assert_eq!(
            read_str_or_text(&doc, &root, SOUND_THEME_KEY).as_deref(),
            Some("Default")
        );
        let low_battery = event_sound_key(SystemSoundEvent::LowBattery);
        assert_eq!(read_bool(&doc, &root, low_battery.as_str()), Some(true));

        let _ = doc.put(&root, SYSTEM_SOUND_VOLUME_KEY, 0.3);
        let _ = doc.put(&root, SOUND_THEME_KEY, "Ocean");
        let _ = doc.put(
            &root,
            event_sound_key(SystemSoundEvent::VolumeChange),
            false,
        );
        pane.sync(&mut doc, &root);
        let saved = SystemPrefs::load().audio.system_sounds;
        assert!((saved.volume - 0.3).abs() < 0.001);
        assert_eq!(saved.theme.as_deref(), Some("ocean"));
        assert!(!saved.events.volume_change);
        assert!(
            saved.events.low_battery,
            "untouched events keep their choice"
        );

        let _ = doc.put(&root, SOUND_THEME_KEY, "Default");
        let _ = doc.put(&root, SYSTEM_SOUNDS_KEY, false);
        pane.sync(&mut doc, &root);
        let saved = SystemPrefs::load().audio.system_sounds;
        assert!(!saved.enabled);
        assert_eq!(
            saved.theme, None,
            "the default theme is stored as no choice"
        );
        assert!(!saved.events.volume_change);

        let _ = doc.put(&root, SOUND_THEME_KEY, "Not installed");
        pane.sync(&mut doc, &root);
        assert_eq!(SystemPrefs::load().audio.system_sounds, saved);
    }
}
