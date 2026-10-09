#![allow(
    clippy::doc_markdown,
    reason = "PipeWire and wpctl are established product names"
)]
//! Audio pane — output volume + mute, microphone input level, device
//! capabilities, diagnostics, and per-application routing.
//!
//! Every control drives real state through an [`AudioEffect`]:
//! - **Volume / Mute** forward to `os_actions` (`wpctl` on the default
//!   sink), the same bindings the compositor's quick-settings panel uses.
//! - **Input level** forwards to the default *source* via `wpctl`
//!   (`@DEFAULT_AUDIO_SOURCE@`) through [`InputControl`].
//! - **Device selection** enumerates the real sinks/sources and runs
//!   `wpctl set-default <id>` through [`DeviceControl`], remembering the
//!   choice in the person's `audio` settings. With no `PipeWire` nothing is
//!   listed, and nothing is fabricated.
//! - **Per-application routing** enumerates the `PipeWire` node graph via
//!   [`media::audio`] (`pw-dump`): each application stream is a row that
//!   routes just that app to a chosen device, with its own volume and mute.
//!   The graph is re-read on a throttle while the pane is open, so a headset
//!   plugged in or an app that starts playing appears.
//!
//! Balance, mono output, echo cancellation and system sounds are the
//! person's `audio` settings; the compositor session applies them.

use std::rc::Rc;
use std::sync::Arc;
use ui::time::{Duration, Instant};

use media::audio::{
    AudioEnumerator, AudioGraph, AudioStream, GraphControl, NoopGraphControl, RealEnumerator,
    RealGraphControl,
};
use media::audio_runtime::{
    AudioRuntimeControl, BoundedSession, MicrophoneMeterSession, RealAudioRuntime,
    RuntimeCapabilities,
};
use media::event_sound::SoundTheme;
use os_actions::Dispatcher;
use settings_store::system_sounds::SystemSoundEvent;
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Dropdown, Row, Slider, Spacer, Text, Toggle, Widget};

use crate::dispatcher::dispatcher_or_real;
use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::prefs::AudioPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    caption, card, choice_row_described, error_caption, error_row, rows, setting_row,
};

/// How often the pane re-reads the `PipeWire` graph while it is open, so a
/// device plugged in (a headset) or an application that just started playing
/// appears without reopening Settings. Cheap enough for an actively-viewed
/// pane; the enumeration is one `pw-dump`.
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(1_500);

/// The picker entry meaning "no explicit device — follow the default".
/// Choosing it clears the stream's `target.object` metadata.
pub(crate) const FOLLOW_DEFAULT: &str = "Default";

impl ui::storage::SettingsSchema for AudioPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("audio");
}

#[ui::tilo_resource::resource(kind = "settings.audio", label = "Sound")]
impl AudioPrefs {}

/// What Sound changes: this device's outputs, inputs and app streams, and
/// the person's audio preferences.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AudioEffect {
    /// Set the output volume, from 0 to 1.
    #[effect(idempotent)]
    SetOutputVolume { volume: f64 },
    /// Mute or unmute the output.
    #[effect(idempotent)]
    SetOutputMuted { muted: bool },
    /// Set the microphone level, from 0 to 1.
    #[effect(idempotent)]
    SetInputVolume { volume: f64 },
    /// Play sound through another output device.
    #[effect(idempotent)]
    SelectOutput {
        #[param("The output device's name")]
        device: String,
    },
    /// Record from another input device.
    #[effect(idempotent)]
    SelectInput {
        #[param("The input device's name")]
        device: String,
    },
    /// Move the sound between left and right: 0 left, 0.5 centred, 1 right.
    #[effect(idempotent)]
    SetBalance { balance: f64 },
    /// Play both channels in each ear.
    #[effect(idempotent)]
    SetMonoOutput { on: bool },
    /// Give calls an echo-cancelled microphone.
    #[effect(idempotent)]
    SetEchoCancellation { on: bool },
    /// How loud system sounds are, from 0 to 1.
    #[effect(idempotent)]
    SetSystemSoundVolume { volume: f64 },
    /// The sound theme system events play from.
    #[effect(idempotent)]
    SetSoundTheme {
        #[param("The theme's name")]
        theme: String,
    },
    /// Whether one kind of system event makes a sound.
    #[effect(idempotent)]
    SetEventSound {
        #[param("The event, by its key")]
        event: String,
        on: bool,
    },
    /// Use another profile of an audio device (high fidelity, headset…).
    #[effect(row, idempotent)]
    SetDeviceProfile {
        #[param("The device's node id")]
        device: u32,
        #[param("The profile's name")]
        profile: String,
    },
    /// Play one app's stream through a device, or `Default` to follow the
    /// system's.
    #[effect(row, idempotent)]
    RouteStream {
        #[param("The stream's node id")]
        stream: u32,
        #[param("The device's name, or Default")]
        device: String,
    },
    /// Set one app's volume, from 0 to 1.
    #[effect(row, idempotent)]
    SetStreamVolume { stream: u32, volume: f64 },
    /// Mute or unmute one app.
    #[effect(row, idempotent)]
    SetStreamMuted { stream: u32, muted: bool },
    /// Show or hide the speaker tests.
    #[effect(view)]
    ShowSoundTests { show: bool },
    /// Play a test tone on the left or right speaker.
    #[effect(label = "Test speaker")]
    TestSpeaker {
        #[param("left or right", choices = ["left", "right"])]
        channel: String,
    },
    /// Show the microphone's level for a few seconds, or stop showing it.
    #[effect(label = "Test microphone")]
    ToggleMicrophoneTest,
}

/// Answer `effect` on the area's model.
#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AudioEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    app.audio()
        .model()
        .borrow_mut()
        .apply(&effect)
        .map(|_| Effected::done())
}

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

/// Run `wpctl args…`, answering its standard output or why it failed.
fn wpctl(args: &[&str]) -> Result<String, String> {
    let out = bounded_command::run("wpctl", args, None, 1024 * 1024, Duration::from_secs(5))
        .map_err(|error| format!("wpctl: {error}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("wpctl exited with {}", out.status)
        } else {
            format!("wpctl: {stderr}")
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Enumerate the audio devices via `wpctl status`, or why they could not be.
fn list_audio_devices() -> Result<AudioDevices, String> {
    wpctl(&["status"]).map(|out| parse_wpctl_status(&out))
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
/// slider rests at the current source level, or why it could not be read.
fn get_source_volume() -> Result<f32, String> {
    let stdout = wpctl(&["get-volume", "@DEFAULT_AUDIO_SOURCE@"])?;
    parse_source_volume(&stdout)
        .ok_or_else(|| format!("wpctl answered an unreadable volume: {}", stdout.trim()))
}

/// `Volume: 0.42` (optionally ` [MUTED]`) as a `0.0..=1.0` fraction.
fn parse_source_volume(stdout: &str) -> Option<f32> {
    let after = stdout.lines().next()?.split(':').nth(1)?.trim();
    after
        .split_whitespace()
        .next()?
        .parse::<f32>()
        .ok()
        .map(|v| v.clamp(0.0, 1.0))
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

/// What the Sound area keeps for the window: the model, read when first
/// needed.
pub struct AudioArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<AudioModel>>,
}

impl AudioArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model, over `PipeWire`, or over a sample graph for a screenshot.
    pub fn model(&self) -> &Shared<AudioModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                AudioModel::of_device(self.cx.clone())
            } else {
                AudioModel::sample(self.cx.clone())
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// One row per app stream, output and input, offering what can be done
    /// to it now.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: AudioEffect| SettingsEffect::from(effect).bound();
        let mut rows = Vec::new();
        for stream in model
            .graph
            .playback_streams
            .iter()
            .chain(&model.graph.record_streams)
        {
            let kind = if stream.playback {
                "settings.audio-playback"
            } else {
                "settings.audio-recording"
            };
            rows.push(
                LiveRow::new(format!("{kind}:{}", stream.id), kind, stream.label())
                    .operation(offer(AudioEffect::SetStreamMuted {
                        stream: stream.id,
                        muted: !stream.muted.unwrap_or(false),
                    }))
                    .operation(offer(AudioEffect::RouteStream {
                        stream: stream.id,
                        device: FOLLOW_DEFAULT.into(),
                    })),
            );
        }
        for (kind, devices) in [
            ("settings.audio-output", &model.devices.sinks),
            ("settings.audio-input", &model.devices.sources),
        ] {
            for device in devices {
                let select = if kind == "settings.audio-output" {
                    AudioEffect::SelectOutput {
                        device: device.name.clone(),
                    }
                } else {
                    AudioEffect::SelectInput {
                        device: device.name.clone(),
                    }
                };
                rows.push(
                    LiveRow::new(format!("{kind}:{}", device.id), kind, device.name.clone())
                        .operation(offer(select)),
                );
            }
        }
        rows
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: what the pane shows, what the device reports, whether it changed"
)]
pub struct AudioModel {
    cx: PaneCx,
    output: Arc<dyn OutputControl>,
    input: Arc<dyn InputControl>,
    device: Arc<dyn DeviceControl>,
    devices: AudioDevices,
    /// The `PipeWire` node graph: the per-application streams the
    /// "Applications" / "Recording" cards route, plus the devices they can
    /// be routed to.
    graph: AudioGraph,
    /// Side-effect surface for editing the graph (route a stream, per-app
    /// volume/mute). Production shells out; tests inject a recorder.
    gctl: Arc<dyn GraphControl>,
    /// Source of fresh graph snapshots for hotplug re-enumeration.
    enumerator: Arc<dyn AudioEnumerator>,
    /// How often to re-enumerate the graph. `MAX` disables it.
    hotplug_interval: Duration,
    last_enum: Option<Instant>,
    runtime: Arc<dyn AudioRuntimeControl>,
    runtime_caps: RuntimeCapabilities,
    /// The person's audio preferences.
    prefs: Followed<AudioPrefs>,
    /// Installed freedesktop sound themes, for the theme picker.
    sound_themes: Vec<SoundTheme>,
    /// Whether this machine can play event sounds at all.
    event_sounds_available: bool,
    tests_shown: bool,
    /// Why the output volume could not be read; the slider and mute switch
    /// are then not offered at a made-up level.
    volume_error: Option<String>,
    /// Why the input level could not be read.
    input_error: Option<String>,
    /// Why the device list could not be read.
    devices_error: Option<String>,
    runtime_notice: Option<String>,
    channel_test: Option<Box<dyn BoundedSession>>,
    mic_session: Option<Box<dyn MicrophoneMeterSession>>,
    mic_level: f32,
    volume: f32,
    input_volume: f32,
    muted: bool,
    changed: bool,
}

impl AudioModel {
    /// This device's audio, read now; a remembered physical route is
    /// applied again when its device is present.
    fn of_device(cx: PaneCx) -> Self {
        let (initial, volume_error) = match os_actions::get_volume_state() {
            Ok(state) => (state, None),
            Err(error) => (
                os_actions::VolumeState {
                    level: 0.0,
                    muted: false,
                },
                Some(error),
            ),
        };
        let (input_volume, input_error) = match get_source_volume() {
            Ok(level) => (level, None),
            Err(error) => (0.0, Some(error)),
        };
        let prefs = cx.follow::<AudioPrefs>();
        let (mut devices, mut devices_error) = match list_audio_devices() {
            Ok(devices) => (devices, None),
            Err(error) => (AudioDevices::default(), Some(error)),
        };
        let mut graph = media::audio::enumerate();
        if let Some(name) = prefs.get().output_device.as_deref()
            && let Some(dev) = graph.sinks.iter().find(|d| d.description == name)
        {
            let id = dev.id;
            if RealGraphControl.set_default_sink(id).is_ok() {
                for sink in &mut graph.sinks {
                    sink.is_default = sink.id == id;
                }
            }
        }
        if let Some(name) = prefs.get().input_device.as_deref()
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
            devices_error = None;
        }
        let runtime: Arc<dyn AudioRuntimeControl> = Arc::new(RealAudioRuntime);
        Self {
            output: Arc::new(SystemOutput(dispatcher_or_real(None))),
            input: Arc::new(RealInput),
            device: Arc::new(RealDevice),
            devices,
            graph,
            gctl: Arc::new(RealGraphControl),
            enumerator: Arc::new(RealEnumerator),
            hotplug_interval: HOTPLUG_INTERVAL,
            last_enum: Some(ui::time::now()),
            runtime_caps: runtime.capabilities(),
            runtime,
            prefs,
            sound_themes: media::event_sound::installed_themes(),
            event_sounds_available: media::event_sound::sound_backend()
                != media::event_sound::SoundBackend::Unavailable,
            tests_shown: false,
            volume_error,
            input_error,
            devices_error,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_level: 0.0,
            volume: initial.level,
            input_volume,
            muted: initial.muted,
            changed: false,
            cx,
        }
    }

    /// A model over `cx`'s settings with no devices and controls that do
    /// nothing; tests give it what they drive.
    fn detached(cx: PaneCx) -> Self {
        Self {
            output: Arc::new(NoopOutput),
            input: Arc::new(NoopInput),
            device: Arc::new(NoopDevice),
            devices: AudioDevices::default(),
            graph: AudioGraph::default(),
            gctl: Arc::new(NoopGraphControl),
            enumerator: Arc::new(StaticEnumerator(AudioGraph::default())),
            hotplug_interval: Duration::MAX,
            last_enum: Some(ui::time::now()),
            runtime: Arc::new(NoopAudioRuntime),
            runtime_caps: RuntimeCapabilities::default(),
            prefs: cx.follow::<AudioPrefs>(),
            sound_themes: Vec::new(),
            event_sounds_available: false,
            tests_shown: false,
            volume_error: None,
            input_error: None,
            devices_error: None,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_level: 0.0,
            volume: 0.5,
            input_volume: 0.75,
            muted: false,
            changed: false,
            cx,
        }
    }

    /// What a screenshot and the crate's tests show: speakers and
    /// headphones, a microphone, and three apps playing or recording.
    fn sample(cx: PaneCx) -> Self {
        use media::audio::AudioDevice as GDevice;
        let sink = |id, serial, name: &str, def| GDevice {
            id,
            serial,
            name: name.to_string(),
            description: name.to_string(),
            is_default: def,
            volume: Some(0.7),
            muted: Some(false),
            channels: 2,
            codec: None,
        };
        let stream = |id, serial, app: &str, media: &str, target, playback, volume| AudioStream {
            id,
            serial,
            app: app.to_string(),
            media_name: Some(media.to_string()),
            playback,
            target_id: Some(target),
            volume: Some(volume),
            muted: Some(false),
        };
        let graph = AudioGraph {
            sinks: vec![
                sink(51, 51, "Speakers", true),
                sink(91, 269, "Headphones", false),
            ],
            sources: vec![sink(52, 52, "Built-in Microphone", true)],
            playback_streams: vec![
                // Routed away from the default onto the Headphones.
                stream(84, 300, "Music", "Nightcall", 91, true, 0.85),
                // Following the default (Speakers).
                stream(90, 320, "Browser", "Conference call", 51, true, 0.60),
            ],
            record_streams: vec![stream(95, 340, "Meeting", "Microphone", 52, false, 0.75)],
            hardware: vec![],
        };
        let mut model = Self::detached(cx);
        model.volume = 0.65;
        model.input_volume = 0.7;
        model.with_graph(graph, Arc::new(NoopGraphControl))
    }

    /// The model over `graph`, editing it through `gctl`; the device
    /// pickers list the graph's devices.
    #[must_use]
    fn with_graph(mut self, graph: AudioGraph, gctl: Arc<dyn GraphControl>) -> Self {
        self.enumerator = Arc::new(StaticEnumerator(graph.clone()));
        if !graph.sinks.is_empty() || !graph.sources.is_empty() {
            self.devices = devices_from_graph(&graph);
        }
        self.graph = graph;
        self.gctl = gctl;
        self
    }

    pub fn devices(&self) -> &AudioDevices {
        &self.devices
    }

    pub fn graph(&self) -> &AudioGraph {
        &self.graph
    }

    /// Carry out `effect`; what it reported.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect, each a short device or preference change"
    )]
    fn apply(&mut self, effect: &AudioEffect) -> Result<String, String> {
        self.changed = true;
        let percent = |value: f64| i64::from((value.clamp(0.0, 1.0) * 100.0).round() as u32);
        match effect {
            AudioEffect::SetOutputVolume { volume } => {
                let volume = volume.clamp(0.0, 1.0) as f32;
                self.output
                    .set_volume(volume * 100.0)
                    .map_err(|error| self.failed("sound-notice-out-vol-err", error))?;
                self.volume = volume;
                Ok(t_args(
                    "sound-notice-out-vol",
                    &[("percent", percent(f64::from(volume)).into())],
                ))
            }
            AudioEffect::SetOutputMuted { muted } => {
                self.output
                    .set_mute(*muted)
                    .map_err(|error| self.failed("sound-notice-mute-err", error))?;
                self.muted = *muted;
                Ok(t(if *muted {
                    "sound-notice-muted"
                } else {
                    "sound-notice-unmuted"
                }))
            }
            AudioEffect::SetInputVolume { volume } => {
                let volume = volume.clamp(0.0, 1.0) as f32;
                self.input
                    .set_input_volume(volume * 100.0)
                    .map_err(|error| self.failed("sound-notice-in-vol-err", error))?;
                self.input_volume = volume;
                Ok(t_args(
                    "sound-notice-in-vol",
                    &[("percent", percent(f64::from(volume)).into())],
                ))
            }
            AudioEffect::SelectOutput { device } => {
                let id = self
                    .devices
                    .sink_id(device)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                self.device
                    .set_default(id)
                    .map_err(|error| self.failed("sound-notice-device-err", error))?;
                for sink in &mut self.devices.sinks {
                    sink.default = sink.id == id;
                }
                let name = device.clone();
                self.save(|prefs| prefs.output_device = Some(name))?;
                Ok(t("sound-notice-out-device"))
            }
            AudioEffect::SelectInput { device } => {
                let id = self
                    .devices
                    .source_id(device)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                self.device
                    .set_default(id)
                    .map_err(|error| self.failed("sound-notice-in-device-err", error))?;
                for source in &mut self.devices.sources {
                    source.default = source.id == id;
                }
                let name = device.clone();
                self.save(|prefs| prefs.input_device = Some(name))?;
                Ok(t("sound-notice-in-device"))
            }
            AudioEffect::SetBalance { balance } => {
                let balance = balance.clamp(0.0, 1.0) as f32;
                self.save(|prefs| prefs.balance = balance)?;
                Ok(t_args(
                    "sound-notice-balance",
                    &[("percent", percent(f64::from(balance)).into())],
                ))
            }
            AudioEffect::SetMonoOutput { on } => {
                let on = *on;
                self.save(|prefs| prefs.mono_output = on)?;
                self.runtime_notice = Some(t(if on {
                    "sound-notice-mono-active"
                } else {
                    "sound-notice-mono-restored"
                }));
                Ok(t(if on {
                    "sound-notice-mono-on"
                } else {
                    "sound-notice-mono-off"
                }))
            }
            AudioEffect::SetEchoCancellation { on } => {
                let on = *on;
                self.save(|prefs| prefs.echo_cancellation = on)?;
                Ok(t(if on {
                    "sound-notice-echo-on"
                } else {
                    "sound-notice-echo-off"
                }))
            }
            AudioEffect::SetSystemSoundVolume { volume } => {
                let volume = volume.clamp(0.0, 1.0);
                self.save_sounds(|sounds| sounds.volume = volume)
            }
            AudioEffect::SetSoundTheme { theme } => {
                let id = self
                    .sound_themes
                    .iter()
                    .find(|known| &known.name == theme || &known.id == theme)
                    .map(|known| known.id.clone())
                    .ok_or_else(|| t("sound-notice-theme-missing"))?;
                self.save_sounds(|sounds| sounds.theme = Some(id))
            }
            AudioEffect::SetEventSound { event, on } => {
                let event = SystemSoundEvent::ALL
                    .into_iter()
                    .find(|known| known.key() == event)
                    .ok_or_else(|| t("sound-notice-event-missing"))?;
                let on = *on;
                self.save_sounds(|sounds| sounds.events.set(event, on))
            }
            AudioEffect::SetDeviceProfile { device, profile } => {
                let index = self
                    .graph
                    .hardware
                    .iter()
                    .find(|hardware| hardware.id == *device)
                    .and_then(|hardware| {
                        hardware
                            .profiles
                            .iter()
                            .find(|known| known.available && &known.description == profile)
                    })
                    .map(|known| known.index)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                self.gctl
                    .set_device_profile(*device, index)
                    .map_err(|error| self.failed("sound-notice-profile-err", error))?;
                Ok(t("sound-notice-profile"))
            }
            AudioEffect::RouteStream { stream, device } => {
                let known = self.stream(*stream)?;
                let result = if is_follow_default(device) {
                    self.gctl.unroute_stream(known.id)
                } else {
                    let devices = if known.playback {
                        &self.graph.sinks
                    } else {
                        &self.graph.sources
                    };
                    devices
                        .iter()
                        .find(|candidate| &candidate.description == device)
                        .ok_or_else(|| "audio route is no longer available".to_string())
                        .and_then(|target| self.gctl.route_stream(known.id, target.serial))
                };
                result.map_err(|error| {
                    warn!(target: "tilo::settings", op = "route_stream", stream, error = %error, "audio stream route failed");
                    t_args("sound-notice-stream-route-err", &[("error", error.into())])
                })?;
                Ok(t("sound-notice-stream-route"))
            }
            AudioEffect::SetStreamVolume { stream, volume } => {
                let known = self.stream(*stream)?;
                self.gctl
                    .set_node_volume(known.id, volume.clamp(0.0, 1.0) as f32)
                    .map_err(|error| {
                        t_args("sound-notice-stream-vol-err", &[("error", error.into())])
                    })?;
                Ok(t("sound-notice-stream-vol"))
            }
            AudioEffect::SetStreamMuted { stream, muted } => {
                let known = self.stream(*stream)?;
                self.gctl.set_node_mute(known.id, *muted).map_err(|error| {
                    t_args("sound-notice-stream-mute-err", &[("error", error.into())])
                })?;
                Ok(t("sound-notice-stream-mute"))
            }
            AudioEffect::ShowSoundTests { show } => {
                self.tests_shown = *show;
                if !show {
                    self.channel_test = None;
                }
                Ok(t(if *show {
                    "sound-notice-diag-on"
                } else {
                    "sound-notice-diag-off"
                }))
            }
            AudioEffect::TestSpeaker { channel } => {
                let left = match channel.as_str() {
                    "left" => true,
                    "right" => false,
                    _ => return Err("the channel is left or right".into()),
                };
                self.channel_test = None;
                let label = t(if left {
                    "sound-test-left"
                } else {
                    "sound-test-right"
                });
                let session = self
                    .graph
                    .default_sink()
                    .filter(|sink| sink.channels == 2)
                    .ok_or_else(|| t("sound-notice-test-need"))
                    .and_then(|sink| self.runtime.start_channel_test(&sink.name, left))
                    .map_err(|error| {
                        let notice = t_args(
                            "sound-notice-test-err",
                            &[("channel", label.as_str().into()), ("error", error.into())],
                        );
                        self.runtime_notice = Some(notice.clone());
                        notice
                    })?;
                self.channel_test = Some(session);
                self.runtime_notice = Some(t_args(
                    "sound-notice-playing",
                    &[("channel", label.as_str().into())],
                ));
                Ok(t_args(
                    "sound-notice-test-started",
                    &[("channel", label.into())],
                ))
            }
            AudioEffect::ToggleMicrophoneTest => {
                if self.mic_session.take().is_some() {
                    self.runtime_notice = Some(t("sound-notice-mic-stopped"));
                    return Ok(t("sound-notice-mic-stopped"));
                }
                let session = self
                    .graph
                    .default_source()
                    .ok_or_else(|| t("sound-notice-mic-none"))
                    .and_then(|source| {
                        self.runtime
                            .start_microphone_meter(&source.name, Duration::from_secs(8))
                    })
                    .map_err(|error| {
                        let notice = t_args("sound-notice-mic-err", &[("error", error.into())]);
                        self.runtime_notice = Some(notice.clone());
                        notice
                    })?;
                self.mic_session = Some(session);
                self.mic_level = 0.0;
                self.runtime_notice = Some(t("sound-notice-mic-started"));
                Ok(t("sound-notice-mic-started"))
            }
        }
    }

    /// The stream `id`, if the graph still has it.
    fn stream(&self, id: u32) -> Result<AudioStream, String> {
        self.graph
            .playback_streams
            .iter()
            .chain(&self.graph.record_streams)
            .find(|stream| stream.id == id)
            .cloned()
            .ok_or_else(|| t("sound-notice-stream-gone"))
    }

    /// `key`'s message for a device call that failed with `error`, shown
    /// on the pane too.
    fn failed(&mut self, key: &str, error: String) -> String {
        warn!(target: "tilo::settings", op = key, error = %error, "audio change failed");
        let notice = t_args(key, &[("error", error.into())]);
        self.runtime_notice = Some(notice.clone());
        notice
    }

    /// Change the person's audio settings.
    fn save(&mut self, change: impl FnOnce(&mut AudioPrefs)) -> Result<(), String> {
        self.prefs.settings().update(change).map_err(|error| {
            t_args(
                "sound-notice-pref-err",
                &[("error", error.to_string().into())],
            )
        })?;
        self.prefs.refresh();
        Ok(())
    }

    /// Change the person's system sounds.
    fn save_sounds(
        &mut self,
        change: impl FnOnce(&mut settings_store::system_sounds::SystemSoundPrefs),
    ) -> Result<String, String> {
        self.save(|prefs| {
            change(&mut prefs.system_sounds);
            prefs.system_sounds = prefs.system_sounds.clone().sanitized();
        })?;
        Ok(t("sound-notice-sounds-ok"))
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
            self.speakers_card(),
            self.build_system_sounds_card(),
        ]);
        crate::widgets::cards(cards)
    }

    /// The way into Speakers: several of your devices as one set.
    fn speakers_card(&self) -> crate::widgets::El {
        let open_label = t("sound-speakers-open");
        let open = self.cx.bind_invocation(
            Button::new("sound.speakers", &open_label),
            crate::effects::NavigationEffect::OpenPane {
                pane: crate::categories::Category::Speakers.slug().to_owned(),
            },
        );
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
    #[expect(
        clippy::too_many_lines,
        reason = "the output card's device, level, balance, mono and test rows"
    )]
    fn build_output_card(&self) -> crate::widgets::El {
        let mut output_rows = Vec::new();
        if let Some(error) = &self.devices_error {
            output_rows.push(error_caption(&t_args(
                "sound-devices-error",
                &[("error", error.as_str().into())],
            )));
        }
        if self.devices.sinks.len() >= 2 {
            let opts: Vec<String> = self.devices.sinks.iter().map(|d| d.name.clone()).collect();
            let current = self.devices.default_sink().unwrap_or_default().to_owned();
            let dd = Dropdown::from_state(opts, &LocalState::new(current.clone()))
                .with_id("audio.output_device")
                .with_placeholder(&current);
            output_rows.push(setting_row(
                &t("sound-output-device"),
                Box::new(self.cx.bind(dd, effect(AudioEffect::SELECT_OUTPUT))),
            ));
        }
        if let Some(error) = &self.volume_error {
            output_rows.push(error_row(
                &t("sound-volume"),
                &t_args("sound-volume-error", &[("error", error.as_str().into())]),
            ));
        } else {
            output_rows.push(setting_row(
                &t("sound-volume"),
                Box::new(
                    self.cx.bind(
                        Slider::from_state(&LocalState::new(f64::from(self.volume)))
                            .with_id("audio.volume"),
                        effect(AudioEffect::SET_OUTPUT_VOLUME),
                    ),
                ),
            ));
            output_rows.push(setting_row(
                &t("sound-mute"),
                Box::new(self.cx.bind(
                    Toggle::from_state(&LocalState::new(self.muted)).with_id("audio.mute"),
                    effect(AudioEffect::SET_OUTPUT_MUTED),
                )),
            ));
        }
        let stereo_sink = self.graph.default_sink().filter(|sink| sink.channels == 2);
        if self.runtime_caps.balance && stereo_sink.is_some() {
            output_rows.push(setting_row(
                &t("sound-balance"),
                Box::new(
                    self.cx.bind(
                        Slider::from_state(&LocalState::new(f64::from(
                            self.prefs.get().balance.clamp(0.0, 1.0),
                        )))
                        .with_id("audio.balance"),
                        effect(AudioEffect::SET_BALANCE),
                    ),
                ),
            ));
        }
        if self.runtime_caps.mono_output && stereo_sink.is_some() {
            output_rows.push(setting_row(
                &t("sound-mono"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(self.prefs.get().mono_output))
                            .with_id("audio.mono"),
                        effect(AudioEffect::SET_MONO_OUTPUT),
                    ),
                ),
            ));
            output_rows.push(caption(&t("sound-mono-caption")));
        }
        if self.runtime_caps.channel_test && stereo_sink.is_some() {
            output_rows.push(Box::new(
                Row::new()
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            "settings.audio.advanced",
                            &if self.tests_shown {
                                t("sound-test-hide")
                            } else {
                                t("sound-test-show")
                            },
                        ),
                        AudioEffect::ShowSoundTests {
                            show: !self.tests_shown,
                        },
                    )))
                    .push(Box::new(Spacer::new())),
            ));
            if self.tests_shown {
                output_rows.push(Box::new(
                    Row::new()
                        .push(Box::new(self.cx.bind_invocation(
                            Button::new("settings.audio.test.left", &t("sound-test-left")),
                            AudioEffect::TestSpeaker {
                                channel: "left".into(),
                            },
                        )))
                        .push(Box::new(self.cx.bind_invocation(
                            Button::new("settings.audio.test.right", &t("sound-test-right")),
                            AudioEffect::TestSpeaker {
                                channel: "right".into(),
                            },
                        )))
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
            let current = self.devices.default_source().unwrap_or_default().to_owned();
            let dd = Dropdown::from_state(opts, &LocalState::new(current.clone()))
                .with_id("audio.input_device")
                .with_placeholder(&current);
            input_rows.push(setting_row(
                &t("sound-input-device"),
                Box::new(self.cx.bind(dd, effect(AudioEffect::SELECT_INPUT))),
            ));
        }
        if let Some(error) = &self.input_error {
            input_rows.push(error_row(
                &t("sound-input-level"),
                &t_args("sound-input-error", &[("error", error.as_str().into())]),
            ));
        } else {
            input_rows.push(setting_row(
                &t("sound-input-level"),
                Box::new(
                    self.cx.bind(
                        Slider::from_state(&LocalState::new(f64::from(self.input_volume)))
                            .with_id("audio.input_volume"),
                        effect(AudioEffect::SET_INPUT_VOLUME),
                    ),
                ),
            ));
        }
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
                    .push(Box::new(
                        Text::new(&activity).with_role(ui::TextRole::Caption),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new(
                            "settings.audio.mic_test",
                            &if running {
                                t("sound-mic-stop")
                            } else {
                                t("sound-mic-test")
                            },
                        ),
                        AudioEffect::ToggleMicrophoneTest,
                    ))),
            ));
            input_rows.push(caption(&t("sound-mic-caption")));
        }
        if self.runtime_caps.echo_cancellation && !self.graph.sources.is_empty() {
            input_rows.push(setting_row(
                &t("sound-echo"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(self.prefs.get().echo_cancellation))
                            .with_id("audio.echo_cancellation"),
                        effect(AudioEffect::SET_ECHO_CANCELLATION),
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
            let device = hw.id;
            let active = hw
                .active_profile
                .and_then(|i| hw.profiles.iter().find(|p| p.index == i))
                .map_or_else(|| t("sound-choose-profile"), |p| p.description.clone());
            hardware_rows.push(choice_row_described(
                &hw.description,
                &t("sound-profile"),
                Box::new(
                    self.cx.bind(
                        Dropdown::from_state(options, &LocalState::new(active.clone()))
                            .with_id(&format!("audio.profile.{}", hw.id))
                            .with_placeholder(&active),
                        effect(AudioEffect::SET_DEVICE_PROFILE)
                            .map(move |profile: String| (device, profile)),
                    ),
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
            let id = s.id;
            let current = self.stream_target_label(s);
            let dd = self.cx.bind(
                Dropdown::from_state(options.clone(), &LocalState::new(current.clone()))
                    .with_id(&format!("audio.stream.{id}.sink.dd"))
                    .with_placeholder(&current),
                effect(AudioEffect::ROUTE_STREAM).map(move |device: String| (id, device)),
            );
            let route_row =
                choice_row_described(&s.app, s.media_name.as_deref().unwrap_or(""), Box::new(dd));
            let vol_row = setting_row(
                &t("sound-volume"),
                Box::new(
                    self.cx.bind(
                        Slider::from_state(&LocalState::new(f64::from(s.volume.unwrap_or(1.0))))
                            .with_id(&format!("audio.stream.{id}.volume.sl")),
                        effect(AudioEffect::SET_STREAM_VOLUME).map(move |volume: f64| (id, volume)),
                    ),
                ),
            );
            let mute_row = setting_row(
                &t("sound-mute"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(s.muted.unwrap_or(false)))
                            .with_id(&format!("audio.stream.{id}.muted.tg")),
                        effect(AudioEffect::SET_STREAM_MUTED).map(move |muted: bool| (id, muted)),
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

    /// The "System sounds" card: the master switch, then volume, theme and
    /// one switch per event while it is on.
    fn build_system_sounds_card(&self) -> crate::widgets::El {
        let sounds = &self.prefs.get().system_sounds;
        if !self.event_sounds_available {
            let reason = match super::installed_programs_unreadable() {
                Some(error) => error_caption(&t_args(
                    "settings-programs-error",
                    &[("error", error.into())],
                )),
                None => caption(&t("sound-system-missing")),
            };
            return card(&t("sound-system"), rows(vec![reason]));
        }
        let mut sound_rows = vec![setting_row(
            &t("sound-system"),
            Box::new(self.cx.bind_field(
                Toggle::from_state(&LocalState::new(sounds.enabled)).with_id("audio.system_sounds"),
                crate::prefs::AudioPrefs::paths().system_sounds.enabled,
            )),
        )];
        if !sounds.enabled {
            return card(&t("sound-system"), rows(sound_rows));
        }
        sound_rows.push(setting_row(
            &t("sound-volume"),
            Box::new(
                self.cx.bind(
                    Slider::from_state(&LocalState::new(sounds.volume))
                        .with_id("audio.system_sound_volume"),
                    effect(AudioEffect::SET_SYSTEM_SOUND_VOLUME),
                ),
            ),
        ));
        if self.sound_themes.len() >= 2 {
            let names = self
                .sound_themes
                .iter()
                .map(|theme| theme.name.clone())
                .collect();
            let current = self
                .sound_themes
                .iter()
                .find(|theme| theme.id == sounds.theme_name())
                .map(|theme| theme.name.clone())
                .unwrap_or_default();
            sound_rows.push(setting_row(
                &t("sound-theme"),
                Box::new(
                    self.cx.bind(
                        Dropdown::from_state(names, &LocalState::new(current))
                            .with_id("audio.sound_theme"),
                        effect(AudioEffect::SET_SOUND_THEME),
                    ),
                ),
            ));
        }
        for event in SystemSoundEvent::ALL {
            let key = event.key().to_owned();
            sound_rows.push(setting_row(
                &event_shown(event),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(sounds.events.get(event)))
                            .with_id(&format!("audio.event_sound.{}", event.key())),
                        effect(AudioEffect::SET_EVENT_SOUND).map(move |on: bool| (key.clone(), on)),
                    ),
                ),
            ));
        }
        sound_rows.push(caption(&t("sound-system-caption")));
        card(&t("sound-system"), rows(sound_rows))
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
        // Pane-owned diagnostics are bound to the old node name. Persistent
        // mono is owned by the compositor session and handles hot-plug there.
        self.channel_test = None;
        self.mic_session = None;
        // Refresh the sink/source pickers from the same enumeration so a new
        // headset is selectable, keeping the current default marker.
        self.devices = devices_from_graph(&fresh);
        self.graph = fresh;
        true
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
            self.changed = true;
        }
        if let Some(session) = &mut self.mic_session {
            let level = session.level();
            if (level - self.mic_level).abs() >= 0.02 {
                self.mic_level = level;
                self.changed = true;
            }
            if !session.running() {
                self.mic_session = None;
                self.runtime_notice = Some(t("sound-notice-mic-done"));
                self.changed = true;
            }
        }
        // Re-read the PipeWire graph on the hotplug throttle so a device
        // plugged in or an app that just started playing appears live.
        if self.poll_hotplug() {
            self.changed = true;
        }
    }
}

impl PaneModel for AudioModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        if self.prefs.refresh() {
            self.changed = true;
        }
        self.tick_sessions_and_hotplug();
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "AudioPane"
    }

    fn busy(&self) -> bool {
        self.mic_session.is_some() || self.channel_test.is_some()
    }

    fn next_wake(&self) -> Option<Instant> {
        (self.hotplug_interval != Duration::MAX)
            .then(|| self.last_enum.unwrap_or_else(ui::time::now) + self.hotplug_interval)
    }
}

/// An input that changes nothing.
struct NoopInput;

impl InputControl for NoopInput {
    fn set_input_volume(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
}

/// The default output's volume and mute.
pub trait OutputControl: Send + Sync {
    /// # Errors
    /// The mixer's message when the volume could not be set.
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    /// # Errors
    /// The mixer's message when the output could not be muted or unmuted.
    fn set_mute(&self, muted: bool) -> Result<(), String>;
}

/// The default output, through the system's dispatcher.
struct SystemOutput(Arc<dyn Dispatcher + Send + Sync>);

impl OutputControl for SystemOutput {
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.0.set_volume(pct)
    }
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.0.set_mute(muted)
    }
}

/// An output that changes nothing.
struct NoopOutput;

impl OutputControl for NoopOutput {
    fn set_volume(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_mute(&self, _: bool) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;
    use media::audio::NoopGraphControl;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records the microphone gains the input slider forwards, so the test
    /// can assert the source volume reached the backend.
    #[derive(Default)]
    struct RecordingInput {
        calls: Mutex<Vec<f32>>,
    }

    /// Levels that could not be read are reported, not drawn as sliders at
    /// a made-up position.
    #[test]
    fn unreadable_levels_are_shown_as_errors() {
        let profile = tempfile::tempdir().unwrap().keep();
        let mut model = AudioModel::detached(PaneCx::in_file(profile.join("settings.json")));
        model.volume_error = Some("wpctl: Could not connect to PipeWire".into());
        model.input_error = Some("wpctl: Could not connect to PipeWire".into());
        model.devices_error = Some("wpctl: Could not connect to PipeWire".into());
        let text = crate::widgets::announced_text(model.compose().as_ref());
        for expected in [
            "Can't read the output volume: wpctl: Could not connect to PipeWire",
            "Can't read the input level: wpctl: Could not connect to PipeWire",
            "Can't list audio devices: wpctl: Could not connect to PipeWire",
        ] {
            assert!(
                text.iter().any(|line| line == expected),
                "{expected}: {text:#?}"
            );
        }
        assert_eq!(parse_source_volume("Volume: 0.42 [MUTED]"), Some(0.42));
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

    fn model() -> AudioModel {
        let profile = tempfile::tempdir().unwrap().keep();
        AudioModel::detached(PaneCx::in_file(profile.join("settings.json")))
    }

    fn recording(rec: &Arc<RecordingDispatcher>) -> AudioModel {
        let mut model = model();
        model.output = Arc::new(SystemOutput(
            Arc::clone(rec) as Arc<dyn Dispatcher + Send + Sync>
        ));
        model
    }

    /// Volume, mute and the input level reach their backends once each.
    #[test]
    fn volume_mute_and_input_reach_their_backends() {
        let rec = Arc::new(RecordingDispatcher::default());
        let input = Arc::new(RecordingInput::default());
        let mut model = recording(&rec);
        model.input = input.clone();
        model
            .apply(&AudioEffect::SetOutputVolume { volume: 0.30 })
            .unwrap();
        model
            .apply(&AudioEffect::SetOutputMuted { muted: true })
            .unwrap();
        model
            .apply(&AudioEffect::SetInputVolume { volume: 0.40 })
            .unwrap();
        let calls = rec.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with("set_volume(") && calls[0].contains("30."));
        assert_eq!(calls[1], "set_mute(true)");
        let inputs = input.calls.lock().unwrap().clone();
        assert_eq!(inputs.len(), 1);
        assert!((inputs[0] - 40.0).abs() < 0.5, "{inputs:?}");
        assert!(model.muted);
    }

    /// wpctl parsing, default sink/source switching, profiles and the
    /// remembered default.
    #[test]
    fn audio_devices_parse_and_default_selection_switches() {
        let d = parse_wpctl_status(sample_status());
        assert_eq!(d.sinks.len(), 2, "sinks: {:?}", d.sinks);
        assert_eq!(d.sources.len(), 2, "sources: {:?}", d.sources);
        assert_eq!(d.sinks[0].id, 51);
        assert_eq!(d.sinks[0].name, "Built-in Audio Analog Stereo");
        assert!(d.sinks[0].default, "the * marks the default sink");
        assert_eq!(d.sinks[1].name, "HDMI / DisplayPort");
        assert!(!d.sinks[1].default);
        assert_eq!(d.default_sink(), Some("Built-in Audio Analog Stereo"));
        assert!(!d.sources.iter().any(|s| s.name.contains("Camera")));
        assert_eq!(d.sources[1].id, 54);
        assert_eq!(parse_wpctl_status(""), AudioDevices::default());

        let dev = Arc::new(RecordingDevice::default());
        let mut model = model();
        model.devices = d;
        model.device = dev.clone();
        model
            .apply(&AudioEffect::SelectOutput {
                device: "HDMI / DisplayPort".into(),
            })
            .unwrap();
        model
            .apply(&AudioEffect::SelectInput {
                device: "USB Microphone".into(),
            })
            .unwrap();
        assert_eq!(dev.calls.lock().unwrap().as_slice(), [52, 54]);
        assert_eq!(
            model
                .cx
                .settings::<AudioPrefs>()
                .get()
                .output_device
                .as_deref(),
            Some("HDMI / DisplayPort"),
            "the chosen output is remembered"
        );
        assert!(
            model
                .apply(&AudioEffect::SelectOutput {
                    device: "Not here".into()
                })
                .is_err()
        );

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
        let mut model = self::model().with_graph(graph, rec.clone());
        model
            .apply(&AudioEffect::SetDeviceProfile {
                device: 7,
                profile: "Headset microphone".into(),
            })
            .unwrap();
        assert_eq!(rec.calls(), vec!["profile(7,2)"]);
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

    fn routing_model(gctl: Arc<dyn GraphControl>) -> AudioModel {
        model().with_graph(graph_two_sinks_one_stream(), gctl)
    }

    /// Bounded diagnostics, hotplugged sinks, and balance/mono settings
    /// without owning the transform.
    #[test]
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
            let mut model = model().with_graph(graph, Arc::new(NoopGraphControl));
            model.runtime_caps = runtime.capabilities();
            model.runtime = runtime.clone();
            model
                .apply(&AudioEffect::SetBalance { balance: 0.75 })
                .unwrap();
            model
                .apply(&AudioEffect::SetMonoOutput { on: true })
                .unwrap();
            model
                .apply(&AudioEffect::TestSpeaker {
                    channel: "left".into(),
                })
                .unwrap();
            model.apply(&AudioEffect::ToggleMicrophoneTest).unwrap();
            assert_eq!(
                runtime.calls.lock().unwrap().as_slice(),
                ["test(alsa_speakers,true)", "meter(alsa_input.mic)",]
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            let prefs = model.cx.settings::<AudioPrefs>().get();
            assert!((prefs.balance - 0.75).abs() < 0.001);
            assert!(prefs.mono_output);
            drop(model);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                2,
                "only bounded tone and mic diagnostics belong to this pane"
            );
        }

        // Case `hotplug_adds_a_newly_plugged_sink`.
        {
            let mut before = graph_two_sinks_one_stream();
            before.sinks.retain(|d| d.description == "Speakers");
            before.playback_streams.clear();
            let after = {
                let mut g = graph_two_sinks_one_stream();
                g.playback_streams.clear();
                g
            };
            let mut model = model().with_graph(before, Arc::new(RecordingGraph::default()));
            model.enumerator = Arc::new(Scripted {
                seq: vec![after],
                idx: Mutex::new(0),
            });
            model.hotplug_interval = Duration::ZERO;
            model.last_enum = None;
            assert_eq!(model.graph().sinks.len(), 1, "starts with one sink");
            assert!(model.poll(), "the plugged-in sink is adopted");
            assert_eq!(model.graph().sinks.len(), 2);
            assert!(model.devices().sinks.iter().any(|d| d.name == "Headphones"));
        }
    }

    /// Routing, unrouting, per-app volume and mute each reach the graph;
    /// a stream that is gone is refused.
    #[test]
    fn per_app_streams_route_and_mix() {
        let rec = Arc::new(RecordingGraph::default());
        let mut model = routing_model(rec.clone());
        assert_eq!(
            model.stream_target_label(&model.graph.playback_streams[0].clone()),
            follow_shown(),
            "a stream on the default sink follows it"
        );
        for effect in [
            AudioEffect::RouteStream {
                stream: 84,
                device: "Headphones".into(),
            },
            AudioEffect::RouteStream {
                stream: 84,
                device: FOLLOW_DEFAULT.into(),
            },
            AudioEffect::SetStreamVolume {
                stream: 84,
                volume: 0.30,
            },
            AudioEffect::SetStreamMuted {
                stream: 84,
                muted: true,
            },
        ] {
            model.apply(&effect).unwrap();
        }
        // Headphones' object.serial is 269 — the routing handle, not its node id.
        assert_eq!(
            rec.calls(),
            vec![
                "route(84,269)",
                "unroute(84)",
                "volume(84,0.30)",
                "mute(84,true)"
            ]
        );
        assert!(
            model
                .apply(&AudioEffect::SetStreamMuted {
                    stream: 999,
                    muted: true
                })
                .is_err()
        );
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
    fn echo_cancellation_and_system_sounds_are_the_persons_settings() {
        let mut model = model();
        assert!(
            model.cx.settings::<AudioPrefs>().get().echo_cancellation,
            "calls are echo-cancelled until the person turns it off"
        );
        model
            .apply(&AudioEffect::SetEchoCancellation { on: false })
            .unwrap();
        assert!(!model.cx.settings::<AudioPrefs>().get().echo_cancellation);

        model.sound_themes = vec![
            SoundTheme {
                id: "freedesktop".into(),
                name: "Default".into(),
            },
            SoundTheme {
                id: "ocean".into(),
                name: "Ocean".into(),
            },
        ];
        model.event_sounds_available = true;
        model
            .apply(&AudioEffect::SetSystemSoundVolume { volume: 0.3 })
            .unwrap();
        model
            .apply(&AudioEffect::SetSoundTheme {
                theme: "Ocean".into(),
            })
            .unwrap();
        model
            .apply(&AudioEffect::SetEventSound {
                event: SystemSoundEvent::VolumeChange.key().into(),
                on: false,
            })
            .unwrap();
        let saved = model.cx.settings::<AudioPrefs>().get().system_sounds;
        assert!((saved.volume - 0.3).abs() < 0.001);
        assert_eq!(saved.theme.as_deref(), Some("ocean"));
        assert!(!saved.events.volume_change);
        assert!(
            saved.events.low_battery,
            "untouched events keep their choice"
        );

        model
            .apply(&AudioEffect::SetSoundTheme {
                theme: "Default".into(),
            })
            .unwrap();
        model
            .cx
            .settings::<AudioPrefs>()
            .update(|prefs| prefs.system_sounds.enabled = false)
            .unwrap();
        let saved = model.cx.settings::<AudioPrefs>().get().system_sounds;
        assert!(!saved.enabled);
        assert_eq!(
            saved.theme, None,
            "the default theme is stored as no choice"
        );
        assert!(
            model
                .apply(&AudioEffect::SetSoundTheme {
                    theme: "Not installed".into()
                })
                .is_err()
        );
        assert_eq!(model.cx.settings::<AudioPrefs>().get().system_sounds, saved);
    }
}
