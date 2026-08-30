//! Audio pane — output volume + mute, microphone input level, and the
//! system-sound preferences.
//!
//! Every control here drives real state:
//! - **Volume / Mute** forward to `os_actions` (`wpctl` on the default
//!   sink), the same bindings the compositor's quick-settings panel uses.
//! - **Input level** forwards to the default *source* via `wpctl`
//!   (`@DEFAULT_AUDIO_SOURCE@`) through [`InputControl`], sampled once on
//!   construction so the slider rests at the mic's current gain.
//! - **Event sounds / chat join / chat leave** are durable preferences
//!   persisted to the shared system-preferences store
//!   ([`crate::prefs::SystemPrefs`]); the compositor's notification layer
//!   reads them when deciding whether to play a sound.
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
//   `audio.event_sounds`      — bool, master system-sound toggle
//   `audio.chat_join_sound`   — bool, play on peer join
//   `audio.chat_leave_sound`  — bool, play on peer leave

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use media::audio::{
    AudioEnumerator, AudioGraph, AudioStream, GraphControl, NoopGraphControl, RealEnumerator,
    RealGraphControl,
};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Slider, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::prefs::{AudioPrefs, SystemPrefs};
use crate::widgets::{caption, card, choice_row_described, rows, setting_row};

/// How often the pane re-reads the `PipeWire` graph while it is open, so a
/// device plugged in (a headset) or an application that just started playing
/// appears without reopening Settings. Cheap enough for an actively-viewed
/// pane; the enumeration is one `pw-dump`.
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(1_500);

/// CRDT key prefix for a per-application stream's routing/volume/mute, keyed by
/// its `PipeWire` node id. `audio.stream.<id>.sink` holds the chosen output
/// label (or [`FOLLOW_DEFAULT`]); `.volume` the perceptual gain; `.muted` the
/// mute.
fn stream_sink_key(id: u32) -> String {
    format!("audio.stream.{id}.sink")
}
fn stream_volume_key(id: u32) -> String {
    format!("audio.stream.{id}.volume")
}
fn stream_muted_key(id: u32) -> String {
    format!("audio.stream.{id}.muted")
}

/// The picker entry meaning "no explicit device — follow the default". Choosing
/// it clears the stream's `target.object` metadata.
const FOLLOW_DEFAULT: &str = "Default";

pub const VOLUME_KEY: &str = "audio.volume";
pub const MUTED_KEY: &str = "audio.muted";
pub const INPUT_VOLUME_KEY: &str = "audio.input_volume";
pub const EVENT_SOUNDS_KEY: &str = "audio.event_sounds";
pub const CHAT_JOIN_KEY: &str = "audio.chat_join_sound";
pub const CHAT_LEAVE_KEY: &str = "audio.chat_leave_sound";
/// CRDT key the output-device (sink) dropdown writes the chosen name into.
pub const OUTPUT_DEVICE_KEY: &str = "audio.output_device";
/// CRDT key the input-device (source) dropdown writes the chosen name into.
pub const INPUT_DEVICE_KEY: &str = "audio.input_device";

/// Side-effect interface for the microphone (default *source*) gain.
/// `os_actions::Dispatcher` only covers the sink, so the pane carries this
/// tiny extra seam; the production impl shells out to `wpctl`, tests inject
/// a recorder.
pub trait InputControl: Send + Sync {
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
    let out = Command::new("wpctl")
        .args(args)
        .output()
        .map_err(|e| format!("wpctl not available: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
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
        if let Some(idx) = name.rfind('[') {
            name = name[..idx].trim().to_string();
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
/// when `wpctl` is missing or fails, so a non-qOS host with no `PipeWire` shows
/// no device pickers rather than fabricated devices.
fn list_audio_devices() -> AudioDevices {
    let Ok(out) = Command::new("wpctl").arg("status").output() else {
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
/// slider rests where the mic actually is. Best-effort; `None` on any
/// failure (no `wpctl`, parse error) so callers fall back to a default.
fn get_source_volume() -> Option<f32> {
    let out = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
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
    initial_volume: f32,
    initial_input: f32,
    initial_prefs: AudioPrefs,
    last_volume: f32,
    last_input: f32,
    last_muted: bool,
    last_prefs: AudioPrefs,
    /// Name of the default sink we last set/seeded, guarding re-dispatch.
    last_sink: Option<String>,
    /// Name of the default source we last set/seeded, guarding re-dispatch.
    last_source: Option<String>,
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
        let initial = os_actions::get_volume().unwrap_or(0.5);
        let initial_input = get_source_volume().unwrap_or(0.75);
        let prefs = SystemPrefs::load().audio;
        let devices = list_audio_devices();
        let last_sink = devices.default_sink().map(str::to_string);
        let last_source = devices.default_source().map(str::to_string);
        // Enumerate the PipeWire graph for the per-application streams (and the
        // devices they route to). Empty on a host with no PipeWire, so the
        // Applications card simply shows its empty state.
        let graph = media::audio::enumerate();
        Self {
            dispatcher: dispatcher_or_real(None),
            input: Arc::new(RealInput),
            device: Arc::new(RealDevice),
            devices,
            graph,
            gctl: Arc::new(RealGraphControl),
            enumerator: Arc::new(RealEnumerator),
            hotplug_interval: HOTPLUG_INTERVAL,
            last_enum: Some(Instant::now()),
            seeded_streams: HashSet::new(),
            last_stream_sink: HashMap::new(),
            last_stream_vol: HashMap::new(),
            last_stream_mute: HashMap::new(),
            initial_volume: initial,
            initial_input,
            initial_prefs: prefs.clone(),
            last_volume: initial,
            last_input: initial_input,
            last_muted: false,
            last_prefs: prefs,
            last_sink,
            last_source,
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
            last_enum: Some(Instant::now()),
            seeded_streams: HashSet::new(),
            last_stream_sink: HashMap::new(),
            last_stream_vol: HashMap::new(),
            last_stream_mute: HashMap::new(),
            initial_volume,
            initial_input,
            initial_prefs: AudioPrefs::default(),
            last_volume: initial_volume,
            last_input: initial_input,
            last_muted: false,
            last_prefs: AudioPrefs::default(),
            last_sink: None,
            last_source: None,
            persist_prefs: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` builder: inject an enumerated device set and the
    /// [`DeviceControl`] recorder so the sink/source picker path is exercised
    /// without shelling out to `wpctl`.
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
    pub fn with_graph(mut self, graph: AudioGraph, gctl: Arc<dyn GraphControl>) -> Self {
        self.enumerator = Arc::new(StaticEnumerator(graph.clone()));
        self.graph = graph;
        self.gctl = gctl;
        self
    }

    /// Test builder: drive the hotplug path from a scripted enumerator with the
    /// throttle disabled, so every `sync` re-enumerates and a device/stream
    /// added to a later snapshot is picked up.
    pub fn with_hotplug(mut self, enumerator: Arc<dyn AudioEnumerator>) -> Self {
        self.enumerator = enumerator;
        self.hotplug_interval = Duration::ZERO;
        self.last_enum = None;
        self
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
        // --- Output card: an optional device picker (only when more than one
        //     sink exists), then volume + mute on the default sink.
        let mut output_rows = Vec::new();
        if self.devices.sinks.len() >= 2 {
            let opts: Vec<String> = self.devices.sinks.iter().map(|d| d.name.clone()).collect();
            let mut dd = Dropdown::new(opts, OUTPUT_DEVICE_KEY).with_id("audio.output_device");
            if let Some(name) = self.devices.default_sink() {
                dd = dd.with_placeholder(name);
            }
            output_rows.push(setting_row("Output device", Box::new(dd)));
        }
        output_rows.push(setting_row(
            "Volume",
            Box::new(Slider::new("settings.audio.volume", VOLUME_KEY).with_id("audio.volume")),
        ));
        output_rows.push(setting_row(
            "Mute",
            Box::new(Toggle::new("settings.audio.mute", MUTED_KEY).with_id("audio.mute")),
        ));
        let output = card("Output", rows(output_rows));

        // --- Input card: an optional source picker, then the mic level.
        let mut input_rows = Vec::new();
        if self.devices.sources.len() >= 2 {
            let opts: Vec<String> = self
                .devices
                .sources
                .iter()
                .map(|d| d.name.clone())
                .collect();
            let mut dd = Dropdown::new(opts, INPUT_DEVICE_KEY).with_id("audio.input_device");
            if let Some(name) = self.devices.default_source() {
                dd = dd.with_placeholder(name);
            }
            input_rows.push(setting_row("Input device", Box::new(dd)));
        }
        input_rows.push(setting_row(
            "Input level",
            Box::new(
                Slider::new("settings.audio.input_volume", INPUT_VOLUME_KEY)
                    .with_id("audio.input_volume"),
            ),
        ));
        let input = card("Input", rows(input_rows));

        // --- Applications card: one block per playback stream, with a picker
        //     that routes just that app to a chosen sink, plus per-app volume +
        //     mute. This is the per-application routing surface — the music app
        //     to the headphones while the browser stays on the speakers.
        let apps = self.streams_card(
            "Applications",
            &self.graph.playback_streams,
            &self.devices.sinks,
            "No applications are playing audio.",
        );
        // --- Recording card: the capture streams (which app is using the mic),
        //     routed to a chosen source, with per-app level + mute.
        let recording = self.streams_card(
            "Recording",
            &self.graph.record_streams,
            &self.devices.sources,
            "No applications are recording.",
        );

        // --- System sounds card: master toggle + per-event collaboration
        //     sounds. Each is a durable preference.
        let sounds = card(
            "System sounds",
            rows(vec![
                setting_row(
                    "Event sounds",
                    Box::new(
                        Toggle::new("settings.audio.event_sounds", EVENT_SOUNDS_KEY)
                            .with_id("audio.event_sounds"),
                    ),
                ),
                setting_row(
                    "Someone joins a chat",
                    Box::new(
                        Toggle::new("settings.audio.chat_join", CHAT_JOIN_KEY)
                            .with_id("audio.chat_join"),
                    ),
                ),
                setting_row(
                    "Someone leaves a chat",
                    Box::new(
                        Toggle::new("settings.audio.chat_leave", CHAT_LEAVE_KEY)
                            .with_id("audio.chat_leave"),
                    ),
                ),
                caption(
                    "Collaboration sounds play when peers join or leave a \
                     shared workspace or chat.",
                ),
            ]),
        );

        crate::widgets::cards(vec![output, input, apps, recording, sounds])
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
        let mut options = vec![FOLLOW_DEFAULT.to_string()];
        options.extend(devices.iter().map(|d| d.name.clone()));

        let mut blocks: Vec<crate::widgets::El> = Vec::new();
        for s in streams {
            let sink_key = stream_sink_key(s.id);
            let vol_key = stream_volume_key(s.id);
            let mute_key = stream_muted_key(s.id);
            let current = self.stream_target_label(s);
            let dd = Dropdown::new(options.clone(), &sink_key)
                .with_id(&format!("{sink_key}.dd"))
                .with_placeholder(&current);
            let route_row =
                choice_row_described(&s.app, s.media_name.as_deref().unwrap_or(""), Box::new(dd));
            let vol_row = setting_row(
                "Volume",
                Box::new(
                    Slider::new("settings.audio.volume", &vol_key)
                        .with_id(&format!("{vol_key}.sl")),
                ),
            );
            let mute_row = setting_row(
                "Mute",
                Box::new(
                    Toggle::new("settings.audio.mute", &mute_key)
                        .with_id(&format!("{mute_key}.tg")),
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
            Some(d) if d.is_default => FOLLOW_DEFAULT.to_string(),
            Some(d) => d.description.clone(),
            None => FOLLOW_DEFAULT.to_string(),
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
    fn dispatch_stream(&mut self, state: &AutoCommit, root: &ObjId, s: &AudioStream) {
        // Route: match the chosen label to a device serial (or follow default).
        if let Some(choice) = read_str(state, root, stream_sink_key(s.id).as_str())
            && self.last_stream_sink.get(&s.id).map(String::as_str) != Some(choice.as_str())
        {
            self.last_stream_sink.insert(s.id, choice.clone());
            let result = if choice == FOLLOW_DEFAULT {
                self.gctl.unroute_stream(s.id)
            } else {
                let devices = if s.playback {
                    &self.graph.sinks
                } else {
                    &self.graph.sources
                };
                match devices.iter().find(|d| d.description == choice) {
                    Some(dev) => self.gctl.route_stream(s.id, dev.serial),
                    None => Ok(()),
                }
            };
            if let Err(e) = result {
                warn!(target: "qos::settings", op = "route_stream", stream = s.id, error = %e, "audio pane stream route failed");
            }
        }
        // Per-app volume.
        if let Some(v) = read_f64(state, root, stream_volume_key(s.id).as_str()) {
            let v = v as f32;
            if (v - self.last_stream_vol.get(&s.id).copied().unwrap_or(v)).abs() > 0.001 {
                self.last_stream_vol.insert(s.id, v);
                if let Err(e) = self.gctl.set_node_volume(s.id, v.clamp(0.0, 1.0)) {
                    warn!(target: "qos::settings", op = "stream_volume", stream = s.id, error = %e, "audio pane stream volume failed");
                }
            }
        }
        // Per-app mute.
        if let Some(b) = read_bool(state, root, stream_muted_key(s.id).as_str())
            && self.last_stream_mute.get(&s.id).copied() != Some(b)
        {
            self.last_stream_mute.insert(s.id, b);
            if let Err(e) = self.gctl.set_node_mute(s.id, b) {
                warn!(target: "qos::settings", op = "stream_mute", stream = s.id, error = %e, "audio pane stream mute failed");
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
        self.last_enum = Some(Instant::now());
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
    fn persist_audio(f: impl FnOnce(&mut AudioPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.audio);
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "audio_prefs_save", error = %e, "failed to persist audio prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AudioPane::new())
}

impl Widget for AudioPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Re-read the PipeWire graph on the hotplug throttle so a device
        // plugged in or an app that just started playing appears live.
        self.poll_hotplug();
        if self.needs_seed {
            let _ = state.put(
                root,
                VOLUME_KEY,
                automerge::ScalarValue::F64(f64::from(self.initial_volume)),
            );
            let _ = state.put(root, MUTED_KEY, automerge::ScalarValue::Boolean(false));
            let _ = state.put(
                root,
                INPUT_VOLUME_KEY,
                automerge::ScalarValue::F64(f64::from(self.initial_input)),
            );
            let _ = state.put(
                root,
                EVENT_SOUNDS_KEY,
                automerge::ScalarValue::Boolean(self.initial_prefs.event_sounds),
            );
            let _ = state.put(
                root,
                CHAT_JOIN_KEY,
                automerge::ScalarValue::Boolean(self.initial_prefs.chat_join),
            );
            let _ = state.put(
                root,
                CHAT_LEAVE_KEY,
                automerge::ScalarValue::Boolean(self.initial_prefs.chat_leave),
            );
            // Seed the device pickers with the current defaults so they rest
            // on the active device.
            if let Some(name) = self.devices.default_sink() {
                let _ = state.put(root, OUTPUT_DEVICE_KEY, name);
            }
            if let Some(name) = self.devices.default_source() {
                let _ = state.put(root, INPUT_DEVICE_KEY, name);
            }
            self.last_prefs = self.initial_prefs.clone();
            self.needs_seed = false;
        }
        if let Some(new) = read_f64(state, root, VOLUME_KEY) {
            let new = new as f32;
            if (new - self.last_volume).abs() > 0.001 {
                self.last_volume = new;
                if let Err(e) = self.dispatcher.set_volume(new.clamp(0.0, 1.0) * 100.0) {
                    warn!(target: "qos::settings", op = "set_volume", error = %e, "audio pane dispatch failed");
                }
            }
        }
        if let Some(new) = read_f64(state, root, INPUT_VOLUME_KEY) {
            let new = new as f32;
            if (new - self.last_input).abs() > 0.001 {
                self.last_input = new;
                if let Err(e) = self.input.set_input_volume(new.clamp(0.0, 1.0) * 100.0) {
                    warn!(target: "qos::settings", op = "set_input_volume", error = %e, "audio pane dispatch failed");
                }
            }
        }
        if let Some(b) = read_bool(state, root, MUTED_KEY)
            && b != self.last_muted
        {
            self.last_muted = b;
            if let Err(e) = self.dispatcher.toggle_mute() {
                warn!(target: "qos::settings", op = "toggle_mute", error = %e, "audio pane dispatch failed");
            }
        }
        // System-sound preferences → persist to the shared store on change.
        if let Some(b) = read_bool(state, root, EVENT_SOUNDS_KEY)
            && b != self.last_prefs.event_sounds
        {
            self.last_prefs.event_sounds = b;
            Self::persist_audio(|a| a.event_sounds = b);
        }
        if let Some(b) = read_bool(state, root, CHAT_JOIN_KEY)
            && b != self.last_prefs.chat_join
        {
            self.last_prefs.chat_join = b;
            Self::persist_audio(|a| a.chat_join = b);
        }
        if let Some(b) = read_bool(state, root, CHAT_LEAVE_KEY)
            && b != self.last_prefs.chat_leave
        {
            self.last_prefs.chat_leave = b;
            Self::persist_audio(|a| a.chat_leave = b);
        }
        // Output-device (sink) selection → wpctl set-default, guarded so a
        // selection at rest doesn't re-fire.
        if let Some(name) = read_str(state, root, OUTPUT_DEVICE_KEY)
            && self.last_sink.as_deref() != Some(name.as_str())
            && let Some(id) = self.devices.sink_id(&name)
        {
            // Persist the chosen default sink so it is remembered across
            // sessions (the preferred output stays selected).
            if self.persist_prefs {
                let nm = name.clone();
                Self::persist_audio(|a| a.output_device = Some(nm));
            }
            self.last_sink = Some(name);
            if let Err(e) = self.device.set_default(id) {
                warn!(target: "qos::settings", op = "set_default_sink", error = %e, "audio pane device switch failed");
            }
        }
        // Input-device (source) selection → wpctl set-default.
        if let Some(name) = read_str(state, root, INPUT_DEVICE_KEY)
            && self.last_source.as_deref() != Some(name.as_str())
            && let Some(id) = self.devices.source_id(&name)
        {
            if self.persist_prefs {
                let nm = name.clone();
                Self::persist_audio(|a| a.input_device = Some(nm));
            }
            self.last_source = Some(name);
            if let Err(e) = self.device.set_default(id) {
                warn!(target: "qos::settings", op = "set_default_source", error = %e, "audio pane device switch failed");
            }
        }
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
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
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
}

/// Read an `f64` scalar at `key`.
fn read_f64(state: &AutoCommit, root: &ObjId, key: &str) -> Option<f64> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::F64(f) => Some(*f),
            _ => None,
        },
        _ => None,
    }
}

/// Read a `bool` scalar at `key`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

/// Read a string at `key` — dropdowns write an Automerge `Text` object, so
/// handle both a plain scalar string and a `Text`.
fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;
    use std::sync::Mutex;

    /// Records the microphone gains the input slider forwards, so the test
    /// can assert the source volume actually reached the backend.
    #[derive(Default)]
    struct RecordingInput {
        calls: Mutex<Vec<f32>>,
    }
    impl InputControl for RecordingInput {
        fn set_input_volume(&self, pct: f32) -> Result<(), String> {
            self.calls.lock().unwrap().push(pct);
            Ok(())
        }
    }

    #[test]
    fn volume_slider_invokes_set_volume_on_drag() {
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

    #[test]
    fn mute_toggle_invokes_toggle_mute() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, MUTED_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        let calls = rec.calls();
        assert_eq!(calls, vec!["toggle_mute()"]);
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

    #[test]
    fn parse_wpctl_status_extracts_sinks_sources_and_default() {
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

    #[test]
    fn parse_wpctl_status_empty_when_no_audio() {
        assert_eq!(parse_wpctl_status(""), AudioDevices::default());
    }

    /// Choosing a different output device runs `set-default` with that sink's
    /// node id exactly once, and not again while it rests.
    #[test]
    fn selecting_output_device_switches_default_once() {
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

    #[test]
    fn selecting_input_device_switches_source_default() {
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

    /// The input-level slider forwards the new gain to the microphone
    /// `InputControl` exactly once per change, and not while it rests.
    #[test]
    fn input_slider_invokes_input_control_on_drag() {
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
        let calls = input.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "one input dispatch per change: {calls:?}");
        assert!(
            (calls[0] - 40.0).abs() < 0.5,
            "expected ~40%, got {}",
            calls[0]
        );
    }

    // --- Per-application routing -------------------------------------------

    use media::audio::{
        AudioDevice as GDevice, AudioGraph as G, AudioStream as GStream, GraphControl,
    };

    /// Records every graph edit the pane dispatches, so a routing/volume/mute
    /// change can be asserted without touching `pw-metadata` / `wpctl`.
    #[derive(Default)]
    struct RecordingGraph {
        calls: Mutex<Vec<String>>,
    }
    impl RecordingGraph {
        fn push(&self, s: String) -> Result<(), String> {
            self.calls.lock().unwrap().push(s);
            Ok(())
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl GraphControl for RecordingGraph {
        fn set_default_sink(&self, id: u32) -> Result<(), String> {
            self.push(format!("default_sink({id})"))
        }
        fn set_default_source(&self, id: u32) -> Result<(), String> {
            self.push(format!("default_source({id})"))
        }
        fn route_stream(&self, stream: u32, serial: u32) -> Result<(), String> {
            self.push(format!("route({stream},{serial})"))
        }
        fn unroute_stream(&self, stream: u32) -> Result<(), String> {
            self.push(format!("unroute({stream})"))
        }
        fn set_node_volume(&self, id: u32, v: f32) -> Result<(), String> {
            self.push(format!("volume({id},{v:.2})"))
        }
        fn set_node_mute(&self, id: u32, m: bool) -> Result<(), String> {
            self.push(format!("mute({id},{m})"))
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
                },
                GDevice {
                    id: 91,
                    serial: 269,
                    name: "alsa_hp".into(),
                    description: "Headphones".into(),
                    is_default: false,
                    volume: Some(1.0),
                    muted: Some(false),
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

    #[test]
    fn stream_seed_rests_without_dispatch() {
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
            read_str(&doc, &root, &stream_sink_key(84)).as_deref(),
            Some(FOLLOW_DEFAULT)
        );
    }

    #[test]
    fn routing_one_app_to_a_sink_moves_only_that_stream_once() {
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

    #[test]
    fn routing_an_app_back_to_default_unroutes_it() {
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

    #[test]
    fn per_app_volume_and_mute_dispatch_once_each() {
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
    fn hotplug_adds_a_newly_plugged_sink() {
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

    #[test]
    fn choosing_a_default_sink_persists_the_preference() {
        // Redirect the config dir so this never touches the real settings.json.
        let _guard = crate::prefs::test_env::ConfigGuard::new("audio-default-device");
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
