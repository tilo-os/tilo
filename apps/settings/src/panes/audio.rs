//! Audio pane — output volume + mute, microphone input level, device
//! capabilities, diagnostics, and per-application routing.
//!
//! Every control drives real state through an [`AudioEffect`]:
//! - **Volume / Mute** forward to `os_actions` (`wpctl` on the default
//!   sink), the same bindings the compositor's quick-settings panel uses.
//! - **Input level** forwards to the default *source* through
//!   [`InputControl`].
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
//! person's `audio` settings; the compositor session applies them. The page
//! is view code over that section ([`page::view`]).

use std::rc::Rc;
use std::sync::Arc;
use ui::time::{Duration, Instant};

use media::audio::default_node::{self, DefaultNode, VolumeState};
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
use provider_settings_ui::bound::SettingRows as _;
use settings_store::system_sounds::SystemSoundEvent;
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource::{self, Effect};

use crate::area::AreaContext;
use crate::dispatcher::dispatcher_or_real;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};

mod level_meter;
mod page;
mod speaker_sets;

pub(crate) use page::view;

use crate::prefs::{AudioDevicePrefs, AudioPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::Item;
use speaker_sets::{NoSpeakerSets, SampleSpeakerSets, SpeakerSets, speaker_call};

crate::effects::effect_family!(AudioEffect => answer);

crate::area::pane_area! {
    AudioArea;
    fn poll(&self) {
        AudioArea::poll(self);
    }
    fn busy(&self) -> bool {
        self.model.get().is_some_and(|model| model.borrow().busy())
    }
    fn next_wake(&self) -> Option<Instant> {
        self.model.get().and_then(|model| model.borrow().next_wake())
    }
}

/// The window's [`AudioArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&AudioArea> {
    app.areas().find()
}

pub(crate) mod values;

/// The switch that mutes the output: the device's, as search shows it.
pub(crate) fn mute(cx: &ViewCx<Settings>, on: bool) -> Item {
    cx.setting(t("sound-mute")).id("audio.mute").toggle_effect(
        &LocalState::new(on),
        &cx.actions(),
        effect(AudioEffect::SET_OUTPUT_MUTED),
    )
}

/// The switch that plays both channels in each ear.
pub(crate) fn mono(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-mono")).id("audio.mono").toggle_verb(
        |prefs: &AudioPrefs| prefs.mono_output,
        AudioPrefs::SET_MONO_OUTPUT,
    )
}

/// The switch that gives calls an echo-cancelled microphone.
pub(crate) fn echo_cancellation(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-echo"))
        .id("audio.echo_cancellation")
        .toggle_verb(
            |prefs: &AudioPrefs| prefs.echo_cancellation,
            AudioPrefs::SET_ECHO_CANCELLATION,
        )
}

/// The switch that lets system events make sounds.
pub(crate) fn system_sounds(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-system"))
        .id("audio.system_sounds")
        .toggle(AudioPrefs::paths().system_sounds.enabled)
}

/// How often the pane re-reads the `PipeWire` graph while it is open, so a
/// device plugged in (a headset) or an application that just started playing
/// appears without reopening Settings. Cheap enough for an actively-viewed
/// pane; the enumeration is one `pw-dump`.
const HOTPLUG_INTERVAL: Duration = Duration::from_millis(1_500);

/// The picker entry meaning "no explicit device — follow the default".
/// Choosing it clears the stream's `target.object` metadata.
pub(crate) const FOLLOW_DEFAULT: &str = "Default";

/// Whether `event` makes a sound, as the sound section holds it.
fn event_sound(event: SystemSoundEvent) -> ui::tilo_resource::Path<AudioPrefs, bool> {
    let events = AudioPrefs::paths().system_sounds.events;
    match event {
        SystemSoundEvent::Notification => events.notification,
        SystemSoundEvent::Message => events.message,
        SystemSoundEvent::VolumeChange => events.volume_change,
        SystemSoundEvent::Screenshot => events.screenshot,
        SystemSoundEvent::DeviceConnected => events.device_connected,
        SystemSoundEvent::DeviceDisconnected => events.device_disconnected,
        SystemSoundEvent::LowBattery => events.low_battery,
        SystemSoundEvent::Error => events.error,
    }
}

/// What Sound changes of this device: its outputs, inputs and app streams,
/// and its bounded tests. The person's audio preferences are the `audio`
/// section's field edits and verbs.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AudioEffect {
    /// Set the output volume, from 0 to 1.
    #[effect(job, idempotent)]
    #[effect(keywords = "volume, loudness, sound level")]
    SetOutputVolume { volume: f64 },
    /// Mute or unmute the output.
    #[effect(job, idempotent)]
    #[effect(keywords = "mute, silence, sound off")]
    SetOutputMuted { muted: bool },
    /// Set the microphone level, from 0 to 1.
    #[effect(job, idempotent)]
    SetInputVolume { volume: f64 },
    /// Play sound through another output device.
    #[effect(job, idempotent)]
    #[effect(keywords = "speakers, headphones, audio output, sound output")]
    SelectOutput {
        #[param("The output device's name")]
        device: String,
    },
    /// Play everything through one of the person's speaker sets, as
    /// through any other output.
    #[effect(job, idempotent)]
    SelectSpeakerSet {
        #[param("The speaker set's id")]
        group: String,
    },
    /// Record from another input device.
    #[effect(job, idempotent)]
    #[effect(keywords = "microphone, mic, audio input")]
    SelectInput {
        #[param("The input device's name")]
        device: String,
    },
    /// Use another profile of an audio device (high fidelity, headset…).
    #[effect(row, job, idempotent)]
    SetDeviceProfile {
        #[param("The device's node id")]
        device: u32,
        #[param("The profile's name")]
        profile: String,
    },
    /// Play one app's stream through a device, or `Default` to follow the
    /// system's.
    #[effect(row, job, idempotent)]
    RouteStream {
        #[param("The stream's node id")]
        stream: u32,
        #[param("The device's name, or Default")]
        device: String,
    },
    /// Set one app's volume, from 0 to 1.
    #[effect(row, job, idempotent)]
    SetStreamVolume { stream: u32, volume: f64 },
    /// Mute or unmute one app.
    #[effect(row, job, idempotent)]
    SetStreamMuted { stream: u32, muted: bool },
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
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let model = crate::panes::audio::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = model.borrow_mut();
    match model.plan(&effect)? {
        Plan::Local => model.apply_local(&effect).map(|_| Effected::done()),
        // `wpctl` and `pw-cli` each take up to their timeout, and a slider
        // drag commits many values: run them off the UI thread, so the
        // click is answered and painted at once.
        Plan::Device { key, work, settle } => {
            let work = model.changes.submit(key, work);
            Ok(app.job(
                cx,
                work,
                move |app: &SettingsApp, ran: Option<Result<(), String>>, _| {
                    // Replaced by a newer change of the same kind before it
                    // ran: that one settles the model.
                    let Some(result) = ran else {
                        return Ok(Effected::done());
                    };
                    crate::panes::audio::area(app)
                        .ok_or_else(crate::area::missing)?
                        .model()
                        .borrow_mut()
                        .settle(settle, result)
                        .map(|_| Effected::done())
                },
            ))
        }
    }
}

/// A change of the audio hardware: one `wpctl` or `pw-cli` run.
type DeviceWork = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// An effect as decided on the UI thread ([`AudioModel::plan`]).
enum Plan {
    /// A diagnostic, started on the UI thread ([`AudioModel::apply_local`]).
    Local,
    /// A device change still to run, keyed by what it changes, and what
    /// the model takes in once it ran.
    Device {
        key: ChangeKey,
        work: DeviceWork,
        settle: Settle,
    },
}

/// What a device change changes: a newer change of the same key replaces
/// one still waiting ([`DeviceChanges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ChangeKey {
    OutputVolume,
    OutputMuted,
    InputVolume,
    OutputDevice,
    InputDevice,
    Profile(u32),
    Route(u32),
    StreamVolume(u32),
    StreamMuted(u32),
}

/// What a device change that ran sets on the model ([`AudioModel::settle`]).
enum Settle {
    OutputVolume(f32),
    OutputMuted(bool),
    InputVolume(f32),
    Output { id: u32, name: String },
    SpeakerSet(String),
    Input { id: u32, name: String },
    Profile,
    Route(u32),
    StreamVolume,
    StreamMuted,
}

/// The device changes of one Sound area, run off the UI thread one at a
/// time. A change a newer one of the same kind replaced before it ran is
/// dropped, so the hardware ends at the last value asked for whatever order
/// the background executor starts them in.
#[derive(Clone, Default)]
struct DeviceChanges {
    latest: Arc<std::sync::Mutex<(u64, std::collections::HashMap<ChangeKey, u64>)>>,
    /// Held while a change runs, so two never race on the mixer.
    turn: Arc<std::sync::Mutex<()>>,
}

impl DeviceChanges {
    /// `work` as a job for the background executor: `None` when a newer
    /// change of `key` replaced it before its turn.
    fn submit(
        &self,
        key: ChangeKey,
        work: DeviceWork,
    ) -> impl FnOnce() -> Option<Result<(), String>> + Send + 'static {
        let ticket = {
            let mut latest = self
                .latest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            latest.0 += 1;
            let ticket = latest.0;
            latest.1.insert(key, ticket);
            ticket
        };
        let this = self.clone();
        move || {
            let _turn = this
                .turn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let current = this
                .latest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .1
                .get(&key)
                == Some(&ticket);
            current.then(work)
        }
    }
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

/// Whether an output's name says it is worn: headphones, a headset or
/// earbuds.
fn device_is_headphones(name: &str) -> bool {
    let name = name.to_lowercase();
    ["headphone", "headset", "earbud", "airpods", "buds"]
        .iter()
        .any(|word| name.contains(word))
}

/// How wide an app's volume slider is, and the narrowest it gets.
const STREAM_SLIDER_WIDTH: f32 = ui::tokens::Metrics::BASE.row_slider_width;
const STREAM_SLIDER_MIN_WIDTH: f32 = ui::tokens::Metrics::BASE.row_slider_min_width;
/// How wide the output volume slider is, with its value beside it.
const OUTPUT_SLIDER_WIDTH: f32 = ui::tokens::Metrics::BASE.row_slider_valued_width;

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

/// Production impl: the default source's level.
pub struct RealInput;

impl InputControl for RealInput {
    fn set_input_volume(&self, pct: f32) -> Result<(), String> {
        default_node::set_volume(DefaultNode::Source, pct)
    }
}

/// One enumerated audio device (a sink or a source) as the graph lists it.
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
        RealGraphControl.set_default_sink(id)
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
/// the sink/source pickers the same way the first read seeded them.
fn devices_from_graph(graph: &AudioGraph) -> AudioDevices {
    let map = |ds: &[media::audio::AudioDevice]| {
        ds.iter()
            // The sink a speaker set plays from is how the set plays, not an
            // output of its own: the set is listed instead.
            .filter(|d| d.name != media::audio::SPEAKER_GROUP_SINK)
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

/// The Sound area's model, shared by the area and the page showing it.
pub type SharedModel = Rc<std::cell::RefCell<AudioModel>>;

/// What the Sound area keeps for the window: the model, read when first
/// needed.
pub struct AudioArea {
    /// Whether it reads and drives this device's audio.
    devices: bool,
    /// The settings document the remembered devices are read from.
    storage: ui::storage::AppStorage,
    /// The sound sections' actions.
    sections: crate::sections::Sections,
    model: std::cell::OnceCell<SharedModel>,
    /// The device's audio, read off the UI thread after the window's first
    /// frame ([`Self::start_read`]); `None` stands for the sample graph a
    /// screenshot shows.
    pub(crate) read: crate::device_read::DeviceRead<Option<AudioRead>>,
    /// Moves whenever what the page shows of the device does: a new
    /// reading, a change it took, a notice, a test starting or ending.
    shown: LocalState<u64>,
    /// The mono output the page last showed, to say what changing it did.
    mono_seen: std::cell::Cell<Option<bool>>,
}

impl AudioArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            devices: cx.devices,
            storage: cx.storage.clone(),
            sections: cx.sections.clone(),
            model: std::cell::OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
            shown: LocalState::new(0),
            mono_seen: std::cell::Cell::new(None),
        }
    }

    /// Take in what moved since the last frame: tests and changes that
    /// ended, the graph a page follows. The page is built again when
    /// anything it shows did.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && model.borrow_mut().poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// The page shows mono output as `on`: once it changed from what the
    /// page showed before, the output card says what that did.
    fn saw_mono(&self, on: bool) {
        if self
            .mono_seen
            .replace(Some(on))
            .is_some_and(|seen| seen != on)
        {
            self.model().borrow_mut().runtime_notice = Some(t(if on {
                "sound-notice-mono-active"
            } else {
                "sound-notice-mono-restored"
            }));
        }
    }

    /// The model, over `PipeWire`, or over a sample graph for a screenshot.
    pub fn model(&self) -> &SharedModel {
        self.model.get_or_init(|| {
            let read = self.read.take().unwrap_or_else(|| self.read_now());
            let model = match read {
                Some(read) => AudioModel::from_read(self.sections.clone(), read),
                None => AudioModel::sample(self.sections.clone()),
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// What a read of the device would find, read on this thread: the
    /// device's audio, or `None` for the sample graph.
    fn read_now(&self) -> Option<AudioRead> {
        let (device, output, input) = self.route();
        device.then(|| AudioRead::of_device(output.as_deref(), input.as_deref()))
    }

    /// Whether the area reaches the device, and the remembered output and
    /// input devices a read routes to again.
    fn route(&self) -> (bool, Option<String>, Option<String>) {
        let prefs = self.storage.settings::<AudioDevicePrefs>().get();
        (self.devices, prefs.output_device, prefs.input_device)
    }

    /// Read the device's audio off the UI thread; `false` when it has been
    /// read or is being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let (device, output, input) = self.route();
        self.read.start(move || {
            device.then(|| AudioRead::of_device(output.as_deref(), input.as_deref()))
        })
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
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
            rows.push(stream_live_row(stream));
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
        // The person's speaker sets are outputs like the others.
        for set in &model.sets.shown {
            rows.push(
                LiveRow::new(
                    format!("settings.audio-output:set:{}", set.id),
                    "settings.audio-output",
                    set.name.clone(),
                )
                .operation(offer(AudioEffect::SelectSpeakerSet {
                    group: set.id.clone(),
                })),
            );
        }
        rows
    }
}

/// `control` acting through `binding`, or shown unavailable without one.
fn bound<I, C: ui::framework::ActionControl<I>>(
    control: C,
    binding: Option<ui::tilo_actions::ActionBinding<I>>,
) -> C {
    match binding {
        Some(binding) => control.bind_action(binding),
        None => control.unavailable(),
    }
}

/// One app's stream as a row of the published surface: its volume, mute
/// and route are operations on it, which the pane's controls for that
/// stream bind to.
fn stream_live_row(stream: &AudioStream) -> LiveRow {
    let offer = |effect: AudioEffect| SettingsEffect::from(effect).bound();
    let kind = if stream.playback {
        "settings.audio-playback"
    } else {
        "settings.audio-recording"
    };
    LiveRow::new(format!("{kind}:{}", stream.id), kind, stream.label())
        .operation(offer(AudioEffect::SetStreamMuted {
            stream: stream.id,
            muted: !stream.muted.unwrap_or(false),
        }))
        .operation(offer(AudioEffect::SetStreamVolume {
            stream: stream.id,
            volume: f64::from(stream.volume.unwrap_or(1.0)),
        }))
        .operation(offer(AudioEffect::RouteStream {
            stream: stream.id,
            device: FOLLOW_DEFAULT.into(),
        }))
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: whether it drives the device, what the runtime offers, what changed"
)]
pub struct AudioModel {
    /// Whether it drives this device's audio: a capture's and a test's
    /// show what they were given.
    reaches_devices: bool,
    /// The sound sections' actions, which remember the devices chosen.
    sections: crate::sections::Sections,
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
    /// Installed freedesktop sound themes, for the theme picker.
    sound_themes: Vec<SoundTheme>,
    /// Whether this machine can play event sounds at all.
    event_sounds_available: bool,
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
    /// The microphone's level while it is tested, for the meter.
    mic_meter: level_meter::Level,
    volume: f32,
    input_volume: f32,
    muted: bool,
    changed: bool,
    /// Device changes on their way off the UI thread.
    changes: DeviceChanges,
    /// The hotplug re-read of the graph, on the background executor.
    rescan: crate::device_read::DeviceRead<AudioGraph>,
    /// While a pane shows the area: `PipeWire`'s own change stream, which
    /// says when to re-read the graph.
    following: std::rc::Weak<GraphMonitor>,
    /// What the output in use plays, for its meter.
    output_meter: level_meter::Level,
    /// While a pane shows the area: the capture behind the output's meter.
    /// Held by the pane, so it ends when the page is left.
    metering: std::rc::Weak<OutputMetering>,
    /// What a page showing the model holds: the graph is followed, and the
    /// speaker sets read, only while one does.
    page: std::rc::Weak<()>,
    /// The person's speaker sets, each an output like any other.
    sets: SpeakerSets,
    /// Where speaker sets are read and chosen: the shell, or a sample.
    speakers: Arc<dyn crate::speaker_setup::SpeakerService>,
}

/// The capture behind the output meter, owned by the pane that shows it:
/// dropping it (leaving the page) ends the capture, so the meter costs
/// nothing while the page is not shown.
#[derive(Default)]
struct OutputMetering {
    /// The output being metered and its capture.
    session: std::cell::RefCell<Option<(String, Box<dyn BoundedSession>)>>,
    /// An output whose capture could not start or ended: not tried again
    /// until the output in use changes.
    refused: std::cell::RefCell<Option<String>>,
}

/// `pw-dump --monitor`, held open while a pane shows the Sound area. It
/// prints each change to the graph as it happens, so the pane re-reads the
/// graph on a change instead of every [`HOTPLUG_INTERVAL`]. (The native
/// registry watch in `media::audio_watch` would link libpipewire into
/// Settings; `pw-dump` is the tool the graph is already read with.)
struct GraphMonitor {
    /// Held for its lifetime: dropping the monitor ends `pw-dump`.
    _process: Option<media::audio::GraphMonitorProcess>,
    changed: Arc<std::sync::atomic::AtomicBool>,
    /// Cleared when the monitor ends; the pane then reads on its timer.
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl GraphMonitor {
    /// Start the monitor; `None` when `pw-dump` cannot run here. It starts
    /// flagged, so the first look reads the graph. Must be called on the UI
    /// thread, in the window's wake scope.
    fn start() -> Option<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (process, stdout) = media::audio::GraphMonitorProcess::start()?;
        let changed = Arc::new(AtomicBool::new(true));
        let alive = Arc::new(AtomicBool::new(true));
        let (flag, running) = (Arc::clone(&changed), Arc::clone(&alive));
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-audio-monitor",
            Box::new(move || {
                use std::io::BufRead as _;
                let mut stdout = std::io::BufReader::new(stdout);
                let mut graph = std::collections::HashSet::new();
                let mut update = Vec::new();
                // Each update is one pretty-printed array, closed by a `]`
                // line of its own.
                while stdout
                    .read_until(b'\n', &mut update)
                    .is_ok_and(|read| read > 0)
                {
                    // An update that never closes is read as a change, not
                    // kept growing.
                    let overlong = update.len() > 16 * 1024 * 1024;
                    if overlong || update.ends_with(b"\n]\n") || update == b"]\n" {
                        let moved = serde_json::from_slice::<serde_json::Value>(&update)
                            .map_or(true, |update| moves_graph(&update, &mut graph));
                        update.clear();
                        if moved {
                            flag.store(true, Ordering::Release);
                            wake.wake();
                        }
                    }
                }
                running.store(false, Ordering::Release);
                wake.wake();
            }),
        );
        // Dropping the process on failure ends it.
        started.ok()?;
        Some(Self {
            _process: Some(process),
            changed,
            alive,
        })
    }

    /// Flagged by hand: no process, never ends.
    #[cfg(test)]
    fn by_hand() -> Self {
        use std::sync::atomic::AtomicBool;
        Self {
            _process: None,
            changed: Arc::new(AtomicBool::new(true)),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether a change waits to be read.
    fn pending(&self) -> bool {
        self.changed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether the graph changed since the last call.
    fn take(&self) -> bool {
        self.changed
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// Whether `update`, one array `pw-dump --monitor` printed, adds, changes
/// or removes a node, a device or the default-device metadata. Clients,
/// ports and links do not count: every `pw-dump` run that re-reads the
/// graph comes and goes as a client, and counting it would re-read forever.
/// `graph` keeps the ids that counted, since a removal names only its id.
fn moves_graph(update: &serde_json::Value, graph: &mut std::collections::HashSet<u64>) -> bool {
    let mut moved = false;
    for object in update.as_array().into_iter().flatten() {
        let Some(id) = object.get("id").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some(
                "PipeWire:Interface:Node"
                | "PipeWire:Interface:Device"
                | "PipeWire:Interface:Metadata",
            ) => {
                graph.insert(id);
                moved = true;
            }
            Some(_) => {}
            None => moved |= graph.remove(&id),
        }
    }
    moved
}

/// What the Sound area reads of the device: the levels, the device list and
/// node graph, what the audio runtime can do and the installed sound
/// themes. That is several `wpctl`/`pw-dump`/`pactl` runs, so the window's
/// warm-up makes the read off the UI thread.
pub(crate) struct AudioRead {
    initial: VolumeState,
    volume_error: Option<String>,
    input_volume: f32,
    input_error: Option<String>,
    devices: AudioDevices,
    devices_error: Option<String>,
    graph: AudioGraph,
    runtime_caps: RuntimeCapabilities,
    sound_themes: Vec<SoundTheme>,
    event_sounds_available: bool,
}

impl AudioRead {
    /// Read this device's audio. The remembered physical route (`output`,
    /// `input`, the devices' names) is applied again when its device is
    /// present.
    fn of_device(output: Option<&str>, input: Option<&str>) -> Self {
        crate::device_read::off_ui_thread();
        let (initial, volume_error) = match media::audio::default_node::volume_state(
            media::audio::default_node::DefaultNode::Sink,
        ) {
            Ok(state) => (state, None),
            Err(error) => (
                VolumeState {
                    level: 0.0,
                    muted: false,
                },
                Some(error),
            ),
        };
        let (input_volume, input_error) = match default_node::volume_state(DefaultNode::Source) {
            Ok(state) => (state.level, None),
            Err(error) => (0.0, Some(error)),
        };
        let (mut graph, devices_error) = match media::audio::try_enumerate() {
            Ok(graph) => (graph, None),
            Err(error) => (AudioGraph::default(), Some(error)),
        };
        if let Some(name) = output
            && let Some(dev) = graph.sinks.iter().find(|d| d.description == name)
        {
            let id = dev.id;
            if RealGraphControl.set_default_sink(id).is_ok() {
                for sink in &mut graph.sinks {
                    sink.is_default = sink.id == id;
                }
            }
        }
        if let Some(name) = input
            && let Some(dev) = graph.sources.iter().find(|d| d.description == name)
        {
            let id = dev.id;
            if RealGraphControl.set_default_source(id).is_ok() {
                for source in &mut graph.sources {
                    source.is_default = source.id == id;
                }
            }
        }
        let devices = devices_from_graph(&graph);
        Self {
            initial,
            volume_error,
            input_volume,
            input_error,
            devices,
            devices_error,
            graph,
            runtime_caps: RealAudioRuntime.capabilities(),
            sound_themes: media::event_sound::installed_themes(),
            event_sounds_available: media::event_sound::sound_backend()
                != media::event_sound::SoundBackend::Unavailable,
        }
    }
}

impl AudioModel {
    /// Whether the output can play both channels in each ear: a stereo
    /// output, on a sound server that mixes it down.
    fn offers_mono(&self) -> bool {
        self.runtime_caps.mono_output
            && self
                .graph
                .default_sink()
                .is_some_and(|sink| sink.channels == 2)
    }

    /// Whether calls can have an echo-cancelled microphone.
    fn offers_echo_cancellation(&self) -> bool {
        self.runtime_caps.echo_cancellation && !self.graph.sources.is_empty()
    }

    /// The model over what `read` found on this device.
    fn from_read(sections: crate::sections::Sections, read: AudioRead) -> Self {
        let AudioRead {
            initial,
            volume_error,
            input_volume,
            input_error,
            devices,
            devices_error,
            graph,
            runtime_caps,
            sound_themes,
            event_sounds_available,
        } = read;
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
            runtime_caps,
            runtime,
            sound_themes,
            event_sounds_available,
            volume_error,
            input_error,
            devices_error,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_meter: level_meter::Level::default(),
            volume: initial.level,
            input_volume,
            muted: initial.muted,
            changed: false,
            changes: DeviceChanges::default(),
            rescan: crate::device_read::DeviceRead::default(),
            following: std::rc::Weak::new(),
            output_meter: level_meter::Level::default(),
            metering: std::rc::Weak::new(),
            page: std::rc::Weak::new(),
            sets: SpeakerSets::default(),
            speakers: Arc::new(crate::speaker_setup::BrokerSpeakers),
            reaches_devices: true,
            sections,
        }
    }

    /// A model with no devices and controls that do nothing, remembering
    /// the devices chosen through `sections`; tests give it what they
    /// drive.
    fn detached(sections: crate::sections::Sections) -> Self {
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
            sound_themes: Vec::new(),
            event_sounds_available: false,
            volume_error: None,
            input_error: None,
            devices_error: None,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_meter: level_meter::Level::default(),
            volume: 0.5,
            input_volume: 0.75,
            muted: false,
            changed: false,
            changes: DeviceChanges::default(),
            rescan: crate::device_read::DeviceRead::default(),
            following: std::rc::Weak::new(),
            output_meter: level_meter::Level::default(),
            metering: std::rc::Weak::new(),
            page: std::rc::Weak::new(),
            sets: SpeakerSets::default(),
            speakers: Arc::new(NoSpeakerSets),
            reaches_devices: false,
            sections,
        }
    }

    /// What a screenshot and the crate's tests show: speakers and
    /// headphones, a microphone, and three apps playing or recording.
    fn sample(sections: crate::sections::Sections) -> Self {
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
        let mut model = Self::detached(sections);
        model.speakers = Arc::new(SampleSpeakerSets);
        model.volume = 0.65;
        model.input_volume = 0.7;
        // Music plays on the speakers: their meter shows it.
        model.output_meter.set(0.46);
        model.output_meter.set_live(true);
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

    /// Carry out `effect` on this thread; what it reported. The window
    /// answers effects through [`answer`], which makes the device changes
    /// off the UI thread instead.
    #[cfg(test)]
    fn apply(&mut self, effect: &AudioEffect) -> Result<String, String> {
        match self.plan(effect)? {
            Plan::Local => self.apply_local(effect),
            Plan::Device { work, settle, .. } => {
                let result = work();
                self.settle(settle, result)
            }
        }
    }

    /// Decide what `effect` does: a preference or diagnostic change made
    /// here, or a device change for [`answer`] to run off the UI thread.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per device effect, each naming its call and what it settles"
    )]
    fn plan(&mut self, effect: &AudioEffect) -> Result<Plan, String> {
        self.changed = true;
        let device = |key: ChangeKey, work: DeviceWork, settle: Settle| -> Result<Plan, String> {
            Ok(Plan::Device { key, work, settle })
        };
        match effect {
            AudioEffect::SetOutputVolume { volume } => {
                let volume = volume.clamp(0.0, 1.0) as f32;
                let output = Arc::clone(&self.output);
                device(
                    ChangeKey::OutputVolume,
                    Box::new(move || output.set_volume(volume * 100.0)),
                    Settle::OutputVolume(volume),
                )
            }
            AudioEffect::SetOutputMuted { muted } => {
                let (output, muted) = (Arc::clone(&self.output), *muted);
                device(
                    ChangeKey::OutputMuted,
                    Box::new(move || output.set_mute(muted)),
                    Settle::OutputMuted(muted),
                )
            }
            AudioEffect::SetInputVolume { volume } => {
                let volume = volume.clamp(0.0, 1.0) as f32;
                let input = Arc::clone(&self.input);
                device(
                    ChangeKey::InputVolume,
                    Box::new(move || input.set_input_volume(volume * 100.0)),
                    Settle::InputVolume(volume),
                )
            }
            AudioEffect::SelectOutput { device: name } => {
                let id = self
                    .devices
                    .sink_id(name)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                let control = Arc::clone(&self.device);
                // Leaving a speaker set: the chosen output first, so the
                // set's sink going away leaves this choice in place.
                let leaving = self
                    .sets
                    .shown
                    .iter()
                    .any(|set| set.output_here)
                    .then(|| Arc::clone(&self.speakers));
                device(
                    ChangeKey::OutputDevice,
                    Box::new(move || {
                        control.set_default(id)?;
                        leaving.map_or(Ok(()), |speakers| {
                            speaker_call(
                                speakers.as_ref(),
                                ui::collaboration::SpeakerCommand::StopUsingAsOutput,
                            )
                        })
                    }),
                    Settle::Output {
                        id,
                        name: name.clone(),
                    },
                )
            }
            AudioEffect::SelectSpeakerSet { group } => {
                let set = self
                    .sets
                    .shown
                    .iter()
                    .find(|set| &set.id == group)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                let (speakers, id) = (Arc::clone(&self.speakers), set.id.clone());
                device(
                    ChangeKey::OutputDevice,
                    Box::new(move || {
                        speaker_call(
                            speakers.as_ref(),
                            ui::collaboration::SpeakerCommand::UseAsOutput { group: id },
                        )
                    }),
                    Settle::SpeakerSet(group.clone()),
                )
            }
            AudioEffect::SelectInput { device: name } => {
                let id = self
                    .devices
                    .source_id(name)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                let control = Arc::clone(&self.device);
                device(
                    ChangeKey::InputDevice,
                    Box::new(move || control.set_default(id)),
                    Settle::Input {
                        id,
                        name: name.clone(),
                    },
                )
            }
            AudioEffect::SetDeviceProfile {
                device: hardware_id,
                profile,
            } => {
                let index = self
                    .graph
                    .hardware
                    .iter()
                    .find(|hardware| hardware.id == *hardware_id)
                    .and_then(|hardware| {
                        hardware
                            .profiles
                            .iter()
                            .find(|known| known.available && &known.description == profile)
                    })
                    .map(|known| known.index)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                let (gctl, hardware_id) = (Arc::clone(&self.gctl), *hardware_id);
                device(
                    ChangeKey::Profile(hardware_id),
                    Box::new(move || gctl.set_device_profile(hardware_id, index)),
                    Settle::Profile,
                )
            }
            AudioEffect::RouteStream {
                stream,
                device: target,
            } => {
                let known = self.stream(*stream)?;
                let gctl = Arc::clone(&self.gctl);
                let work: DeviceWork = if is_follow_default(target) {
                    Box::new(move || gctl.unroute_stream(known.id))
                } else {
                    let devices = if known.playback {
                        &self.graph.sinks
                    } else {
                        &self.graph.sources
                    };
                    let serial = devices
                        .iter()
                        .find(|candidate| &candidate.description == target)
                        .map(|candidate| candidate.serial);
                    Box::new(move || {
                        serial
                            .ok_or_else(|| "audio route is no longer available".to_string())
                            .and_then(|serial| gctl.route_stream(known.id, serial))
                    })
                };
                device(ChangeKey::Route(*stream), work, Settle::Route(*stream))
            }
            AudioEffect::SetStreamVolume { stream, volume } => {
                let known = self.stream(*stream)?;
                let (gctl, volume) = (Arc::clone(&self.gctl), volume.clamp(0.0, 1.0) as f32);
                device(
                    ChangeKey::StreamVolume(*stream),
                    Box::new(move || gctl.set_node_volume(known.id, volume)),
                    Settle::StreamVolume,
                )
            }
            AudioEffect::SetStreamMuted { stream, muted } => {
                let known = self.stream(*stream)?;
                let (gctl, muted) = (Arc::clone(&self.gctl), *muted);
                device(
                    ChangeKey::StreamMuted(*stream),
                    Box::new(move || gctl.set_node_mute(known.id, muted)),
                    Settle::StreamMuted,
                )
            }
            _ => Ok(Plan::Local),
        }
    }

    /// What a device change that ran makes of the model: the level or
    /// device it set, or the notice of why it failed.
    fn settle(&mut self, settle: Settle, result: Result<(), String>) -> Result<String, String> {
        self.changed = true;
        let percent = |value: f32| i64::from((value.clamp(0.0, 1.0) * 100.0).round() as u32);
        match settle {
            Settle::OutputVolume(volume) => {
                result.map_err(|error| self.failed("sound-notice-out-vol-err", error))?;
                self.volume = volume;
                Ok(t_args(
                    "sound-notice-out-vol",
                    &[("percent", percent(volume).into())],
                ))
            }
            Settle::OutputMuted(muted) => {
                result.map_err(|error| self.failed("sound-notice-mute-err", error))?;
                self.muted = muted;
                Ok(t(if muted {
                    "sound-notice-muted"
                } else {
                    "sound-notice-unmuted"
                }))
            }
            Settle::InputVolume(volume) => {
                result.map_err(|error| self.failed("sound-notice-in-vol-err", error))?;
                self.input_volume = volume;
                Ok(t_args(
                    "sound-notice-in-vol",
                    &[("percent", percent(volume).into())],
                ))
            }
            Settle::Output { id, name } => {
                result.map_err(|error| self.failed("sound-notice-device-err", error))?;
                for sink in &mut self.devices.sinks {
                    sink.default = sink.id == id;
                }
                for set in &mut self.sets.shown {
                    set.output_here = false;
                }
                self.sets.due = true;
                self.remember(AudioDevicePrefs::REMEMBER_OUTPUT.with(name))?;
                Ok(t("sound-notice-out-device"))
            }
            Settle::SpeakerSet(group) => {
                result.map_err(|error| self.failed("sound-notice-device-err", error))?;
                // The set's sink is the output now: no device of this one is.
                for sink in &mut self.devices.sinks {
                    sink.default = false;
                }
                for set in &mut self.sets.shown {
                    set.output_here = set.id == group;
                }
                self.sets.due = true;
                Ok(t("sound-notice-out-device"))
            }
            Settle::Input { id, name } => {
                result.map_err(|error| self.failed("sound-notice-in-device-err", error))?;
                for source in &mut self.devices.sources {
                    source.default = source.id == id;
                }
                self.remember(AudioDevicePrefs::REMEMBER_INPUT.with(name))?;
                Ok(t("sound-notice-in-device"))
            }
            Settle::Profile => {
                result.map_err(|error| self.failed("sound-notice-profile-err", error))?;
                Ok(t("sound-notice-profile"))
            }
            Settle::Route(stream) => {
                result.map_err(|error| {
                    warn!(target: "tilo::settings", op = "route_stream", stream, error = %error, "audio stream route failed");
                    t_args("sound-notice-stream-route-err", &[("error", error.into())])
                })?;
                Ok(t("sound-notice-stream-route"))
            }
            Settle::StreamVolume => {
                result.map_err(|error| {
                    t_args("sound-notice-stream-vol-err", &[("error", error.into())])
                })?;
                Ok(t("sound-notice-stream-vol"))
            }
            Settle::StreamMuted => {
                result.map_err(|error| {
                    t_args("sound-notice-stream-mute-err", &[("error", error.into())])
                })?;
                Ok(t("sound-notice-stream-mute"))
            }
        }
    }

    /// Carry out an effect that changes no hardware: a bounded diagnostic.
    fn apply_local(&mut self, effect: &AudioEffect) -> Result<String, String> {
        match effect {
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
                    self.mic_meter.set_live(false);
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
                self.mic_meter.set_live(true);
                self.runtime_notice = Some(t("sound-notice-mic-started"));
                Ok(t("sound-notice-mic-started"))
            }
            AudioEffect::SetOutputVolume { .. }
            | AudioEffect::SetOutputMuted { .. }
            | AudioEffect::SetInputVolume { .. }
            | AudioEffect::SelectOutput { .. }
            | AudioEffect::SelectSpeakerSet { .. }
            | AudioEffect::SelectInput { .. }
            | AudioEffect::SetDeviceProfile { .. }
            | AudioEffect::RouteStream { .. }
            | AudioEffect::SetStreamVolume { .. }
            | AudioEffect::SetStreamMuted { .. } => {
                Err("a device change is planned, not applied here".into())
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

    /// Remember the device chosen, through the `audio-device` section's
    /// verb.
    fn remember(&self, invoke: ui::tilo_resource::Invoke<AudioDevicePrefs>) -> Result<(), String> {
        self.sections
            .invoke(invoke)
            .map_err(|error| t_args("sound-notice-pref-err", &[("error", error.into())]))
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

    /// Re-enumerate the graph on the hotplug throttle. When the topology
    /// changed (a device plugged/unplugged, an app started/stopped playing),
    /// adopt the new graph, refresh the device pickers, and drop the composed
    /// tree so the next layout rebuilds with the new rows. Returns whether a
    /// structural change was adopted.
    fn poll_hotplug(&mut self) -> bool {
        // `pw-dump` can take up to its timeout, so the graph is read on the
        // background executor and taken in by a later sync; scrolling the
        // pane never waits for it.
        let fresh = if self.rescan.is_finished()
            && let Some(graph) = self.rescan.take()
        {
            graph
        } else {
            let monitor = self
                .following
                .upgrade()
                .filter(|monitor| monitor.is_alive());
            let due = match (monitor, self.hotplug_interval, self.last_enum) {
                // A change during a re-read stays flagged for the next one.
                (Some(monitor), ..) => self.rescan.is_finished() && monitor.take(),
                (None, i, _) if i == Duration::ZERO => true,
                (None, i, _) if i == Duration::MAX => false,
                (None, i, Some(t)) => t.elapsed() >= i,
                (None, _, None) => true,
            };
            if !due || !self.rescan.is_finished() {
                return false;
            }
            self.last_enum = Some(ui::time::now());
            let enumerator = Arc::clone(&self.enumerator);
            if self.rescan.start(move || enumerator.enumerate()) {
                return false;
            }
            // No background executor runs here: read on this thread.
            self.enumerator.enumerate()
        };
        if fresh.topology_signature() == self.graph.topology_signature() {
            return false;
        }
        // Pane-owned diagnostics are bound to the old node name. Persistent
        // mono is owned by the compositor session and handles hot-plug there.
        self.channel_test = None;
        self.mic_session = None;
        self.mic_meter.set_live(false);
        // Refresh the sink/source pickers from the same enumeration so a new
        // headset is selectable, keeping the current default marker.
        self.devices = devices_from_graph(&fresh);
        self.graph = fresh;
        // A set taking or leaving the output adds or removes its sink.
        self.sets.due = true;
        true
    }

    /// Read the speaker sets again when they are due, and take in a read
    /// that ended. Whether what the page shows changed.
    fn poll_speaker_sets(&mut self) -> bool {
        self.sets.poll(&self.speakers)
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
            // The meter reads the level itself each frame; the page is not
            // rebuilt for it.
            self.mic_meter.set(session.level());
            if !session.running() {
                self.mic_session = None;
                self.mic_meter.set_live(false);
                self.runtime_notice = Some(t("sound-notice-mic-done"));
                self.changed = true;
            }
        }
        // While a page shows the model, re-read the PipeWire graph on a
        // change (or the hotplug throttle) so a device plugged in or an app
        // that just started playing appears live.
        if self.page.upgrade().is_some() {
            if self.poll_hotplug() {
                self.changed = true;
            }
            if self.poll_speaker_sets() {
                self.changed = true;
            }
        }
        self.follow_output_meter();
    }

    /// Keep the output meter on the output in use while a pane shows it:
    /// start its capture, move it when the default output changes, and let
    /// it go when it ends. Without a pane nothing is captured.
    fn follow_output_meter(&self) {
        if !self.reaches_devices {
            // A capture or a test shows the level it was given.
            return;
        }
        let Some(metering) = self.metering.upgrade() else {
            self.output_meter.set_live(false);
            return;
        };
        let sink = self
            .graph
            .default_sink()
            .map(|sink| sink.name.clone())
            .filter(|_| self.runtime_caps.microphone_meter);
        let mut session = metering.session.borrow_mut();
        if let Some((name, running)) = session.as_mut() {
            if Some(&*name) == sink.as_ref() && running.running() {
                return;
            }
            if running.running() {
                // The output in use changed: meter the new one.
                *session = None;
            } else {
                *metering.refused.borrow_mut() = Some(name.clone());
                *session = None;
            }
        }
        self.output_meter.set_live(false);
        let Some(sink) = sink else {
            return;
        };
        if metering.refused.borrow().as_ref() == Some(&sink) {
            return;
        }
        let publish = self.output_meter.publisher(ui::WakeHandle::new());
        match self.runtime.start_output_meter(&sink, Box::new(publish)) {
            Ok(started) => {
                *session = Some((sink, started));
                self.output_meter.set_live(true);
            }
            Err(error) => {
                tracing::debug!(target: "tilo::settings", %error, "the output meter could not start");
                *metering.refused.borrow_mut() = Some(sink);
            }
        }
    }
}

impl AudioModel {
    /// Take in what moved: tests that ended and, while a page shows the
    /// model, the graph and the speaker sets. Whether what the page shows
    /// changed since the last call, by this or by an effect.
    fn poll(&mut self) -> bool {
        self.tick_sessions_and_hotplug();
        std::mem::take(&mut self.changed)
    }

    /// Whether a test is running that the window keeps drawing for.
    fn busy(&self) -> bool {
        self.mic_session.is_some() || self.channel_test.is_some()
    }

    /// When a page showing the model next re-reads the graph while nothing
    /// else happens.
    fn next_wake(&self) -> Option<Instant> {
        self.page.upgrade()?;
        if let Some(monitor) = self
            .following
            .upgrade()
            .filter(|monitor| monitor.is_alive())
        {
            // A change that came during a re-read is read once it ends (its
            // end wakes the window).
            return (monitor.pending() && self.rescan.is_finished()).then(ui::time::now);
        }
        (self.hotplug_interval != Duration::MAX)
            .then(|| self.last_enum.unwrap_or_else(ui::time::now) + self.hotplug_interval)
    }

    /// Start following the device for a page that shows the model:
    /// `PipeWire`'s change stream and the output's meter run while a page
    /// holds what this answers. Pages showing the model share one, so
    /// building a page again never restarts them.
    fn follow(&mut self) -> Rc<dyn std::any::Any> {
        let page = crate::watch::shared(&mut self.page, || ());
        if !self.reaches_devices {
            return page;
        }
        let metering = self.metering.upgrade().unwrap_or_else(|| {
            let metering = Rc::new(OutputMetering::default());
            self.metering = Rc::downgrade(&metering);
            metering
        });
        let monitor = self.following.upgrade().or_else(|| {
            let monitor = Rc::new(GraphMonitor::start()?);
            self.following = Rc::downgrade(&monitor);
            Some(monitor)
        });
        self.follow_output_meter();
        Rc::new((page, monitor, metering))
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

    type App = ui::framework::testing::TestApp<crate::SettingsShell>;

    /// Change the window's Sound model as `change` does, for the page to
    /// show.
    fn change_model(app: &mut App, change: impl FnOnce(&mut AudioModel) + 'static) {
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            let mut model = area(settings).unwrap().model().borrow_mut();
            change(&mut model);
            model.changed = true;
        })
        .expect("the Settings window is mounted");
    }

    /// What the window shows once it took in what changed.
    fn frame(app: &mut App) -> String {
        for _ in 0..4 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        app.harness().inspection_json().unwrap()
    }

    /// The window, on the Sound page.
    fn on_sound() -> App {
        ui::i18n::I18nManager::init("en-US");
        let mut app = App::new().unwrap();
        app.harness().resize((1200, 6000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Sound);
        app
    }

    /// Levels that could not be read are reported, not drawn as sliders at
    /// a made-up position; when nothing can be read, the failure is said
    /// once rather than once per row.
    #[test]
    fn unreadable_levels_are_shown_as_errors() {
        let mut app = on_sound();
        let error = "wpctl: Could not connect to PipeWire";
        change_model(&mut app, move |model| {
            model.volume_error = Some(error.into());
        });
        let volume = "Can't read the output volume: wpctl: Could not connect to PipeWire";
        let text = frame(&mut app);
        assert!(text.contains(volume), "{text}");

        change_model(&mut app, move |model| {
            model.input_error = Some(error.into());
            model.devices_error = Some(error.into());
        });
        let text = frame(&mut app);
        let devices = "Can't list audio devices: wpctl: Could not connect to PipeWire";
        assert!(text.contains(devices), "{text}");
        assert!(!text.contains(volume), "{text}");
        assert!(
            !text.contains(&t("sound-apps-empty")),
            "no list of apps that only looks silent: {text}"
        );
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

    fn sample_devices() -> AudioDevices {
        let device = |id, name: &str, default| AudioDevice {
            id,
            name: name.into(),
            default,
        };
        AudioDevices {
            sinks: vec![
                device(51, "Built-in Audio Analog Stereo", true),
                device(52, "HDMI / DisplayPort", false),
            ],
            sources: vec![
                device(53, "Built-in Audio Analog Stereo", true),
                device(54, "USB Microphone", false),
            ],
        }
    }

    fn model() -> AudioModel {
        AudioModel::detached(crate::sections::Sections::default())
    }

    fn recording(rec: &Arc<RecordingDispatcher>) -> AudioModel {
        let mut model = model();
        model.output = Arc::new(SystemOutput(
            Arc::clone(rec) as Arc<dyn Dispatcher + Send + Sync>
        ));
        model
    }

    /// The shell's speaker lane, recording what Settings asks; the set is
    /// the output once it was chosen, until it is left.
    #[derive(Default)]
    struct RecordingSpeakers {
        commands: Mutex<Vec<ui::collaboration::SpeakerCommand>>,
        output_here: std::sync::atomic::AtomicBool,
    }

    impl crate::speaker_setup::SpeakerService for RecordingSpeakers {
        fn call(
            &self,
            command: ui::collaboration::SpeakerCommand,
        ) -> Result<ui::collaboration::SpeakerReply, String> {
            use ui::collaboration::{SpeakerCommand, SpeakerGroupInfo, SpeakerReply};
            match &command {
                SpeakerCommand::UseAsOutput { .. } => {
                    self.output_here.store(true, Ordering::SeqCst);
                }
                SpeakerCommand::StopUsingAsOutput => {
                    self.output_here.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
            let snapshot = matches!(command, SpeakerCommand::Snapshot);
            self.commands.lock().unwrap().push(command);
            Ok(if snapshot {
                SpeakerReply::Snapshot {
                    snapshot: ui::collaboration::SpeakerSnapshot {
                        media_enabled: true,
                        groups: vec![SpeakerGroupInfo {
                            id: "cd".repeat(16),
                            name: "Living room".into(),
                            leader_label: "Desk".into(),
                            led_here: true,
                            members: Vec::new(),
                            playback: None,
                            microphones: Vec::new(),
                            output_here: self.output_here.load(Ordering::SeqCst),
                        }],
                        ..ui::collaboration::SpeakerSnapshot::default()
                    },
                }
            } else {
                SpeakerReply::Done { group: None }
            })
        }
    }

    /// A speaker set is listed among the outputs (the sink it plays from
    /// is not), choosing it plays through it, and choosing another output
    /// leaves it, remembering that output.
    #[test]
    fn a_speaker_set_is_an_output_chosen_like_any_other() {
        use media::audio::AudioDevice as GDevice;
        use ui::collaboration::SpeakerCommand;
        let sink = |id, name: &str, description: &str, default| GDevice {
            id,
            serial: id,
            name: name.into(),
            description: description.into(),
            is_default: default,
            volume: Some(0.5),
            muted: Some(false),
            channels: 2,
            codec: None,
        };
        let graph = AudioGraph {
            sinks: vec![
                sink(51, "alsa_output.speakers", "Speakers", true),
                // The sink a set plays from is not an output of its own.
                sink(77, media::audio::SPEAKER_GROUP_SINK, "Speaker group", false),
            ],
            ..AudioGraph::default()
        };
        let speakers = Arc::new(RecordingSpeakers::default());
        let mut app = on_sound();
        {
            let speakers = Arc::clone(&speakers);
            change_model(&mut app, move |model| {
                let devices = devices_from_graph(&graph);
                model.devices = devices;
                model.graph = graph;
                model.speakers = speakers;
                model.sets = SpeakerSets::default();
            });
        }
        let lists_the_set = |app: &mut App| {
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness
                        .inspection_json()
                        .is_ok_and(|frame| frame.contains("Living room"))
                })
        };
        assert!(lists_the_set(&mut app), "the set is in the output list");
        let text = frame(&mut app);
        assert!(text.contains("Speakers"), "{text}");
        assert!(!text.contains("Speaker group"), "{text}");

        app.effect(&SettingsEffect::from(AudioEffect::SelectSpeakerSet {
            group: "cd".repeat(16),
        }))
        .expect("the set is chosen");
        assert!(
            speakers
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::UseAsOutput {
                    group: "cd".repeat(16)
                })
        );
        let model = |app: &mut App, read: fn(&AudioModel) -> bool| {
            app.with_widget::<crate::window::Inspected, _>(move |settings| {
                read(&area(settings).unwrap().model().borrow())
            })
            .unwrap()
        };
        assert!(
            model(&mut app, |model| model.sets.shown[0].output_here),
            "the set is the output in use"
        );
        assert!(
            model(&mut app, |model| model
                .devices()
                .sinks
                .iter()
                .all(|sink| !sink.default)),
            "and no other output is"
        );

        let before = speakers.commands.lock().unwrap().len();
        app.effect(&SettingsEffect::from(AudioEffect::SelectOutput {
            device: "Speakers".into(),
        }))
        .expect("the output is chosen");
        assert!(
            speakers.commands.lock().unwrap()[before..]
                .contains(&SpeakerCommand::StopUsingAsOutput),
            "choosing another output leaves the set"
        );
        assert!(!model(&mut app, |model| model.sets.shown[0].output_here));
        assert!(model(&mut app, |model| model
            .devices()
            .sinks
            .iter()
            .any(|sink| sink.default)));
        let _ = frame(&mut app);
        assert_eq!(
            app.settings::<AudioDevicePrefs>()
                .get()
                .output_device
                .as_deref(),
            Some("Speakers"),
            "the chosen output is remembered"
        );
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

    /// Default sink/source switching, profiles and the remembered default.
    #[test]
    fn default_selection_switches() {
        let d = sample_devices();
        assert_eq!(d.default_sink(), Some("Built-in Audio Analog Stereo"));

        let dev = Arc::new(RecordingDevice::default());
        let mut app = on_sound();
        {
            let dev = Arc::clone(&dev);
            change_model(&mut app, move |model| {
                model.devices = d;
                model.device = dev;
            });
        }
        app.effect(&SettingsEffect::from(AudioEffect::SelectOutput {
            device: "HDMI / DisplayPort".into(),
        }))
        .unwrap();
        app.effect(&SettingsEffect::from(AudioEffect::SelectInput {
            device: "USB Microphone".into(),
        }))
        .unwrap();
        assert_eq!(dev.calls.lock().unwrap().as_slice(), [52, 54]);
        let _ = frame(&mut app);
        let remembered = app.settings::<AudioDevicePrefs>().get();
        assert_eq!(
            remembered.output_device.as_deref(),
            Some("HDMI / DisplayPort"),
            "the chosen output is remembered"
        );
        assert_eq!(remembered.input_device.as_deref(), Some("USB Microphone"));
        assert!(
            app.effect(&SettingsEffect::from(AudioEffect::SelectOutput {
                device: "Not here".into()
            }))
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
            let profile = tempfile::tempdir().unwrap();
            let stored = crate::area::AreaContext::in_profile(profile.path().to_path_buf())
                .storage
                .settings::<AudioPrefs>();
            stored.update(|prefs| prefs.balance = 0.75).unwrap();
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
            let prefs = stored.get();
            assert!((prefs.balance - 0.75).abs() < 0.001);
            drop(model);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                2,
                "only bounded tone and mic diagnostics belong to this pane"
            );
        }

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
            let _page = model.follow();
            assert_eq!(model.graph().sinks.len(), 1, "starts with one sink");
            // The re-read runs on the background executor: the sync that
            // starts it returns at once, and a later sync adopts it.
            assert!(
                !model.poll(),
                "the sync starting the re-read waits for nothing"
            );
            let deadline = tilo_test_env::hang_deadline();
            while !model.poll() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the plugged-in sink is adopted"
                );
                std::thread::yield_now();
            }
            assert_eq!(model.graph().sinks.len(), 2);
            assert!(model.devices().sinks.iter().any(|d| d.name == "Headphones"));
        }
    }

    /// A shown pane re-reads the graph when `PipeWire` reports a change,
    /// never on a timer; once the monitor ends it reads on the timer again.
    #[test]
    fn a_followed_graph_is_read_on_a_change_not_on_a_timer() {
        let mut before = graph_two_sinks_one_stream();
        before.sinks.retain(|d| d.description == "Speakers");
        let after = graph_two_sinks_one_stream();
        let mut model = model().with_graph(before, Arc::new(RecordingGraph::default()));
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        model.enumerator = Arc::new(Counting {
            graph: after,
            reads: Arc::clone(&reads),
        });
        model.hotplug_interval = HOTPLUG_INTERVAL;
        model.last_enum = None;
        let page = model.follow();
        let monitor = Rc::new(GraphMonitor::by_hand());
        model.following = Rc::downgrade(&monitor);
        assert!(monitor.take(), "the monitor starts flagged");
        let adopt = |model: &mut AudioModel| {
            let deadline = tilo_test_env::hang_deadline();
            while !model.poll() {
                assert!(std::time::Instant::now() < deadline, "the read lands");
                std::thread::yield_now();
            }
        };

        for _ in 0..3 {
            assert!(!model.poll());
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0, "no change, no re-read");
        assert_eq!(model.next_wake(), None, "an idle pane sets no deadline");

        monitor.changed.store(true, Ordering::Release);
        adopt(&mut model);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "one change, one re-read");
        assert_eq!(model.graph().sinks.len(), 2, "the new headset is shown");
        assert_eq!(model.next_wake(), None);

        monitor.alive.store(false, Ordering::Release);
        assert!(
            model.next_wake().is_some(),
            "a monitor that ended leaves the pane on its timer"
        );
        drop(page);
        assert_eq!(model.next_wake(), None, "no page, no timer");
    }

    /// Only nodes, devices and the default-device metadata move the graph;
    /// the clients and links every `pw-dump` run brings do not, and a
    /// removal counts only for an id that counted.
    #[test]
    fn only_nodes_devices_and_defaults_move_the_graph() {
        let mut graph = std::collections::HashSet::new();
        let update = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
        assert!(!moves_graph(
            &update(
                r#"[{"id":90,"type":"PipeWire:Interface:Client","info":{}},
                       {"id":91,"type":"PipeWire:Interface:Link","info":{}}]"#
            ),
            &mut graph
        ));
        assert!(!moves_graph(
            &update(r#"[{"id":90,"info":null}]"#),
            &mut graph
        ));
        assert!(moves_graph(
            &update(r#"[{"id":60,"type":"PipeWire:Interface:Node","info":{}}]"#),
            &mut graph
        ));
        assert!(moves_graph(
            &update(r#"[{"id":31,"type":"PipeWire:Interface:Metadata","metadata":[]}]"#),
            &mut graph
        ));
        assert!(moves_graph(
            &update(r#"[{"id":60,"info":null}]"#),
            &mut graph
        ));
        assert!(!moves_graph(
            &update(r#"[{"id":60,"info":null}]"#),
            &mut graph
        ));
    }

    struct Counting {
        graph: AudioGraph,
        reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl AudioEnumerator for Counting {
        fn enumerate(&self) -> AudioGraph {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.graph.clone()
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

    /// Mono output, echo cancellation and the system sounds are the
    /// person's `audio` section, changed by its verbs and field edits as the
    /// page's controls change them.
    #[test]
    fn echo_cancellation_and_system_sounds_are_the_persons_settings() {
        use crate::testing::{edit_path, invoke_section};
        const AUDIO: &str = "settings.audio";
        let mut app = on_sound();
        assert!(
            app.settings::<AudioPrefs>().get().echo_cancellation,
            "calls are echo-cancelled until the person turns it off"
        );
        invoke_section(
            &mut app,
            AUDIO,
            "set-echo-cancellation",
            serde_json::json!({ "on": false }),
        )
        .unwrap();
        assert!(!app.settings::<AudioPrefs>().get().echo_cancellation);
        invoke_section(
            &mut app,
            AUDIO,
            "set-mono-output",
            serde_json::json!({ "on": true }),
        )
        .unwrap();
        assert!(app.settings::<AudioPrefs>().get().mono_output);

        edit_path(&mut app, AUDIO, &["system_sounds", "volume"], 0.3).unwrap();
        edit_path(&mut app, AUDIO, &["system_sounds", "theme"], "ocean").unwrap();
        edit_path(
            &mut app,
            AUDIO,
            &["system_sounds", "events", "volume_change"],
            false,
        )
        .unwrap();
        let saved = app.settings::<AudioPrefs>().get().system_sounds;
        assert!((saved.volume - 0.3).abs() < 0.001);
        assert_eq!(saved.theme.as_deref(), Some("ocean"));
        assert!(!saved.events.volume_change);
        assert!(
            saved.events.low_battery,
            "untouched events keep their choice"
        );
        assert!(
            edit_path(&mut app, AUDIO, &["mono_output"], false).is_err(),
            "mono output changes only through its verb"
        );

        // The page's switches and choosers are bound to those edits.
        change_model(&mut app, |model| {
            model.event_sounds_available = true;
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
        });
        let _ = frame(&mut app);
        let bound: Vec<String> =
            ui::widget::controls::control_action_bindings(app.harness().root_widget())
                .into_iter()
                .filter(|binding| {
                    matches!(
                        &binding.kind,
                        ui::widget::ControlBindingKind::Action(address)
                            if address.resource == format!("settings:{AUDIO}")
                    )
                })
                .map(|binding| binding.control_id)
                .collect();
        for control in [
            "audio.system_sounds",
            "audio.system_sound_volume",
            "audio.sound_theme",
            "audio.event_sound.low_battery",
        ] {
            assert!(
                bound.iter().any(|id| id == control),
                "{control} changes the section: {bound:?}"
            );
        }

        // Turning mono output off while the page is open says what it did.
        invoke_section(
            &mut app,
            AUDIO,
            "set-mono-output",
            serde_json::json!({ "on": false }),
        )
        .unwrap();
        let text = frame(&mut app);
        assert!(text.contains(&t("sound-notice-mono-restored")), "{text}");

        edit_path(&mut app, AUDIO, &["system_sounds", "enabled"], false).unwrap();
        let text = frame(&mut app);
        assert!(
            !text.contains(&t("sound-theme")),
            "with system sounds off only their switch is shown: {text}"
        );
    }

    /// A mixer that does not answer until it is let go, as `wpctl` does
    /// against a wedged `PipeWire`.
    struct StalledOutput(Arc<crate::stall::Stall>);

    impl StalledOutput {
        fn wait(&self) -> Result<(), String> {
            if self.0.wait() {
                Ok(())
            } else {
                Err("the mixer never answered".into())
            }
        }
    }

    impl OutputControl for StalledOutput {
        fn set_volume(&self, _: f32) -> Result<(), String> {
            self.wait()
        }
        fn set_mute(&self, _: bool) -> Result<(), String> {
            self.wait()
        }
    }

    /// A click on the volume control is answered and the window keeps
    /// drawing while the mixer takes its time: the device call runs off
    /// the UI thread, and its result lands once it returns.
    #[test]
    fn a_stalled_mixer_never_holds_the_window() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            crate::panes::audio::area(settings)
                .unwrap()
                .model()
                .borrow_mut()
                .output = Arc::new(StalledOutput(stalled));
        })
        .expect("the Settings window is mounted");

        app.start_effect(&SettingsEffect::from(AudioEffect::SetOutputVolume {
            volume: 0.3,
        }))
        .expect("the volume change runs as a job");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("the click");
        let shows_set_level = |app: &mut ui::framework::testing::TestApp<crate::SettingsShell>| {
            app.with_widget::<crate::window::Inspected, _>(|settings| {
                (crate::panes::audio::area(settings)
                    .unwrap()
                    .model()
                    .borrow()
                    .volume
                    - 0.3)
                    .abs()
                    < 0.01
            })
            .unwrap()
        };
        assert!(
            !shows_set_level(&mut app),
            "the level is not shown as set before the mixer took it"
        );

        gate.release();
        let deadline = tilo_test_env::hang_deadline();
        while !shows_set_level(&mut app) {
            assert!(
                std::time::Instant::now() < deadline,
                "the level the mixer took is shown"
            );
            app.settle();
            std::thread::yield_now();
        }
    }

    /// Changes of one kind that the background executor starts out of
    /// order leave the hardware at the last one asked for.
    #[test]
    fn a_replaced_device_change_is_dropped() {
        let changes = DeviceChanges::default();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let job = |level: u32| -> DeviceWork {
            let ran = Arc::clone(&ran);
            Box::new(move || {
                ran.lock().unwrap().push(level);
                Ok(())
            })
        };
        let first = changes.submit(ChangeKey::OutputVolume, job(20));
        let second = changes.submit(ChangeKey::OutputVolume, job(80));
        let mute = changes.submit(ChangeKey::OutputMuted, job(0));
        assert_eq!(second(), Some(Ok(())));
        assert_eq!(first(), None, "the older level never reaches the mixer");
        assert_eq!(mute(), Some(Ok(())), "another kind of change is kept");
        assert_eq!(ran.lock().unwrap().as_slice(), [80, 0]);
    }
}
