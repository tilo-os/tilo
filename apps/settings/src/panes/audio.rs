//! Audio pane — output volume + mute, microphone input level, device
//! capabilities, diagnostics, and per-application routing. Everything the pane
//! shows and changes is the session's `sound` resource
//! ([`tilo_audio_service::sound`]): its readings say which outputs and inputs
//! are there, which one plays, how loud, the applications' streams and the
//! sound cards' profiles, and every control takes one of its actions.

use std::rc::Rc;
use std::sync::Arc;
use ui::time::Duration;

use media::audio_runtime::{
    AudioRuntimeControl, BoundedSession, MicrophoneMeterSession, RealAudioRuntime,
    RuntimeCapabilities,
};
use media::event_sound::SoundTheme;
use provider_settings_ui::bound::SettingRows as _;
use settings_store::system_sounds::SystemSoundEvent;
use tilo_audio_service::sound::{Device, SOUND, Sound, SoundAction, Stream};
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect};

use crate::area::AreaContext;
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
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&AudioArea> {
    app.areas().find()
}

pub(crate) mod values;

pub(crate) fn mute(cx: &ViewCx<Settings>, on: bool) -> Item {
    cx.setting(t("sound-mute")).id("audio.mute").toggle_effect(
        &LocalState::new(on),
        &cx.actions(),
        effect(AudioEffect::SET_OUTPUT_MUTED),
    )
}

pub(crate) fn mono(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-mono")).id("audio.mono").toggle_verb(
        |prefs: &AudioPrefs| prefs.mono_output,
        AudioPrefs::SET_MONO_OUTPUT,
    )
}

pub(crate) fn echo_cancellation(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-echo"))
        .id("audio.echo_cancellation")
        .toggle_verb(
            |prefs: &AudioPrefs| prefs.echo_cancellation,
            AudioPrefs::SET_ECHO_CANCELLATION,
        )
}

pub(crate) fn system_sounds(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-system"))
        .id("audio.system_sounds")
        .toggle(AudioPrefs::paths().system_sounds.enabled)
}

pub(crate) const FOLLOW_DEFAULT: &str = "Default";

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
    TestSpeaker {
        #[param("left or right", choices = ["left", "right"])]
        channel: String,
    },
    /// Show the microphone's level for a few seconds, or stop showing it.
    #[effect(label = "sound-mic-test")]
    ToggleMicrophoneTest,
}

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
    let (key, work, settle) = match model.plan(&effect)? {
        Plan::Local => return model.apply_local(&effect).map(|_| Effected::done()),
        Plan::Sound {
            key,
            action,
            settle,
        } => (key, model.invoking(action), settle),
        Plan::Change { key, work, settle } => (key, work, settle),
    };
    // A change takes up to its timeout, and a slider drag commits many
    // values: run them off the UI thread, so the click is answered and
    // painted at once.
    let work = model.changes.submit(key, work);
    Ok(app.job(
        cx,
        work,
        move |app: &SettingsApp, ran: Option<Result<(), String>>, _| {
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

type ChangeWork = Box<dyn FnOnce() -> Result<(), String> + Send>;

enum Plan {
    Local,
    Sound {
        key: ChangeKey,
        action: SoundAction,
        settle: Settle,
    },
    Change {
        key: ChangeKey,
        work: ChangeWork,
        settle: Settle,
    },
}

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

enum Settle {
    OutputVolume(f32),
    OutputMuted(bool),
    InputVolume(f32),
    Output { id: u32, name: String },
    SpeakerSet(String),
    Input { id: u32, name: String },
    Profile,
    Route,
    StreamVolume,
    StreamMuted,
}

/// The changes of one Sound area, run off the UI thread one at a time. A
/// change a newer one of the same kind replaced before it ran is dropped, so
/// the sound ends at the last value asked for whatever order the background
/// executor starts them in.
#[derive(Clone, Default)]
struct DeviceChanges {
    latest: Arc<std::sync::Mutex<(u64, std::collections::HashMap<ChangeKey, u64>)>>,
    turn: Arc<std::sync::Mutex<()>>,
}

impl DeviceChanges {
    fn submit(
        &self,
        key: ChangeKey,
        work: ChangeWork,
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

fn device_is_headphones(name: &str) -> bool {
    let name = name.to_lowercase();
    ["headphone", "headset", "earbud", "airpods", "buds"]
        .iter()
        .any(|word| name.contains(word))
}

const STREAM_SLIDER_WIDTH: f32 = ui::tokens::Metrics::BASE.row_slider_width;
const STREAM_SLIDER_MIN_WIDTH: f32 = ui::tokens::Metrics::BASE.row_slider_min_width;
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

pub trait SoundResource: Send + Sync {
    fn latest(&self) -> Option<Result<String, String>>;
    fn attach(&self, wake: ui::WakeHandle);
    fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String>;
}

struct SessionSound(ui::collaboration::SessionResource);

impl SessionSound {
    fn open() -> Result<Self, String> {
        ui::collaboration::SessionResource::open(SOUND).map(Self)
    }
}

impl SoundResource for SessionSound {
    fn latest(&self) -> Option<Result<String, String>> {
        self.0.latest()
    }

    fn attach(&self, wake: ui::WakeHandle) {
        self.0.attach(wake);
    }

    fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        self.0.invoke(action, arguments)
    }
}

struct SampleSound(std::sync::Mutex<Option<Result<String, String>>>);

impl SoundResource for SampleSound {
    fn latest(&self) -> Option<Result<String, String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn attach(&self, _: ui::WakeHandle) {}

    fn invoke(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub type SharedModel = Rc<std::cell::RefCell<AudioModel>>;

pub struct AudioArea {
    devices: bool,
    storage: ui::storage::AppStorage,
    sections: crate::sections::Sections,
    model: std::cell::OnceCell<SharedModel>,
    pub(crate) read: crate::device_read::DeviceRead<Option<AudioRead>>,
    shown: LocalState<u64>,
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

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && model.borrow_mut().poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

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

    fn read_now(&self) -> Option<AudioRead> {
        self.route().then(|| AudioRead::of_session(&self.storage))
    }

    fn route(&self) -> bool {
        self.devices
    }

    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let device = self.route();
        let storage = self.storage.clone();
        self.read
            .start(move || device.then(|| AudioRead::of_session(&storage)))
    }

    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: AudioEffect| SettingsEffect::from(effect).bound();
        let mut rows = Vec::new();
        for stream in &model.sound.streams {
            rows.push(stream_live_row(stream));
        }
        for (kind, devices) in [
            ("settings.audio-output", &model.sound.outputs),
            ("settings.audio-input", &model.sound.inputs),
        ] {
            for device in devices {
                let select = if kind == "settings.audio-output" {
                    AudioEffect::SelectOutput {
                        device: device.label.clone(),
                    }
                } else {
                    AudioEffect::SelectInput {
                        device: device.label.clone(),
                    }
                };
                rows.push(
                    LiveRow::new(
                        format!("{kind}:{}", device.node),
                        kind,
                        device.label.clone(),
                    )
                    .operation(offer(select)),
                );
            }
        }
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

fn bound<I, C: ui::framework::ActionControl<I>>(
    control: C,
    binding: Option<ui::tilo_actions::ActionBinding<I>>,
) -> C {
    match binding {
        Some(binding) => control.bind_action(binding),
        None => control.unavailable(),
    }
}

fn stream_live_row(stream: &Stream) -> LiveRow {
    let offer = |effect: AudioEffect| SettingsEffect::from(effect).bound();
    let kind = if stream.playback {
        "settings.audio-playback"
    } else {
        "settings.audio-recording"
    };
    let row = LiveRow::new(format!("{kind}:{}", stream.node), kind, stream.label());
    let row = match stream.plays() {
        Some(plays) => row.presentation(ResourcePresentation {
            detail: Some(plays.to_owned()),
            badge: None,
            preview: None,
        }),
        None => row,
    };
    row.operation(offer(AudioEffect::SetStreamMuted {
        stream: stream.node,
        muted: !stream.muted,
    }))
    .operation(offer(AudioEffect::SetStreamVolume {
        stream: stream.node,
        volume: f64::from(stream.volume.unwrap_or(1.0)),
    }))
    .operation(offer(AudioEffect::RouteStream {
        stream: stream.node,
        device: FOLLOW_DEFAULT.into(),
    }))
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: whether it drives the device, what the runtime offers, what changed"
)]
pub struct AudioModel {
    reaches_devices: bool,
    sections: crate::sections::Sections,
    sounds: Option<Arc<dyn SoundResource>>,
    sound: Sound,
    runtime: Arc<dyn AudioRuntimeControl>,
    runtime_caps: RuntimeCapabilities,
    sound_themes: Vec<SoundTheme>,
    event_sounds_available: bool,
    /// Why the resource could not be reached; the controls then are not
    /// offered at made-up levels.
    volume_error: Option<String>,
    input_error: Option<String>,
    devices_error: Option<String>,
    runtime_notice: Option<String>,
    channel_test: Option<Box<dyn BoundedSession>>,
    mic_session: Option<Box<dyn MicrophoneMeterSession>>,
    mic_meter: level_meter::Level,
    volume: f32,
    input_volume: f32,
    muted: bool,
    changed: bool,
    changes: DeviceChanges,
    output_meter: level_meter::Level,
    /// While a pane shows the area: the capture behind the output's meter.
    /// Held by the pane, so it ends when the page is left.
    metering: std::rc::Weak<OutputMetering>,
    /// What a page showing the model holds: the readings are taken in, and
    /// the speaker sets read, only while one does.
    page: std::rc::Weak<()>,
    sets: SpeakerSets,
    speakers: Arc<dyn crate::speaker_setup::SpeakerService>,
}

/// The capture behind the output meter, owned by the pane that shows it:
/// dropping it (leaving the page) ends the capture, so the meter costs
/// nothing while the page is not shown.
#[derive(Default)]
struct OutputMetering {
    session: std::cell::RefCell<Option<(String, Box<dyn BoundedSession>)>>,
    refused: std::cell::RefCell<Option<String>>,
}

/// What the Sound area reads: the session's sound resource, the reading it
/// began at, and what the audio runtime here can do. Opening the resource
/// waits for that reading, so the window's warm-up makes the read off the UI
/// thread.
pub(crate) struct AudioRead {
    sounds: Option<Arc<dyn SoundResource>>,
    sound: Sound,
    /// Why the session could not be reached: no reading is shown, and
    /// nothing is made up in its place.
    error: Option<String>,
    runtime_caps: RuntimeCapabilities,
    sound_themes: Vec<SoundTheme>,
    event_sounds_available: bool,
}

impl AudioRead {
    fn of_session(storage: &ui::storage::AppStorage) -> Self {
        crate::device_read::off_ui_thread();
        let remembered = storage.settings::<AudioDevicePrefs>().get();
        let (sounds, sound, error) = match SessionSound::open() {
            Ok(resource) => {
                let reading = resource.latest().unwrap_or_else(|| {
                    Err("The session's sound resource published no reading.".to_string())
                });
                match reading.and_then(|reading| reading_of(&reading)) {
                    Ok(sound) => {
                        restore_choice(&resource, &sound, &remembered);
                        let sound = resource
                            .latest()
                            .and_then(Result::ok)
                            .and_then(|reading| reading_of(&reading).ok())
                            .unwrap_or(sound);
                        (
                            Some(Arc::new(resource) as Arc<dyn SoundResource>),
                            sound,
                            None,
                        )
                    }
                    Err(error) => (
                        Some(Arc::new(resource) as Arc<dyn SoundResource>),
                        Sound::default(),
                        Some(error),
                    ),
                }
            }
            Err(error) => (None, Sound::default(), Some(error)),
        };
        Self {
            sounds,
            sound,
            error,
            runtime_caps: RealAudioRuntime.capabilities(),
            sound_themes: media::event_sound::installed_themes(),
            event_sounds_available: media::event_sound::sound_backend()
                != media::event_sound::SoundBackend::Unavailable,
        }
    }
}

fn restore_choice(resource: &impl SoundResource, sound: &Sound, remembered: &AudioDevicePrefs) {
    let output = remembered.output_device.as_deref().and_then(|name| {
        sound
            .outputs
            .iter()
            .find(|device| device.label == name && !device.chosen)
            .map(|device| device.node)
    });
    let input = remembered.input_device.as_deref().and_then(|name| {
        sound
            .inputs
            .iter()
            .find(|device| device.label == name && !device.chosen)
            .map(|device| device.node)
    });
    for action in [
        output.map(|node| SoundAction::RouteOutput { node }),
        input.map(|node| SoundAction::SelectInput { node }),
    ]
    .into_iter()
    .flatten()
    {
        let id = action.id();
        if let Err(error) = resource.invoke(id, &action.arguments()) {
            tracing::debug!(target: "tilo::settings", op = id, %error, "a remembered device was not put in use");
        }
    }
}

fn device(node: u32, label: &str, chosen: bool) -> Device {
    Device {
        node,
        name: label.to_lowercase().replace(' ', "-"),
        label: label.to_owned(),
        chosen,
        volume: Some(0.8),
        muted: false,
        channels: 2,
        codec: None,
    }
}

fn stream(
    node: u32,
    app: &str,
    media: &str,
    target: Option<u32>,
    playback: bool,
    volume: f32,
) -> Stream {
    Stream {
        node,
        app: app.to_owned(),
        media: Some(media.to_owned()),
        playback,
        target,
        volume: Some(volume),
        muted: false,
    }
}

fn reading_of(reading: &str) -> Result<Sound, String> {
    serde_json::from_str(reading)
        .map_err(|error| format!("The session's sound reading is not a sound: {error}"))
}

impl AudioModel {
    fn offers_mono(&self) -> bool {
        self.runtime_caps.mono_output
            && self
                .output_device()
                .is_some_and(|device| device.channels == 2)
    }

    fn offers_echo_cancellation(&self) -> bool {
        self.runtime_caps.echo_cancellation && !self.sound.inputs.is_empty()
    }

    fn from_read(sections: crate::sections::Sections, read: AudioRead) -> Self {
        let AudioRead {
            sounds,
            sound,
            error,
            runtime_caps,
            sound_themes,
            event_sounds_available,
        } = read;
        if let Some(sounds) = sounds.as_deref() {
            sounds.attach(ui::WakeHandle::new());
        }
        let reaches_devices = sounds.is_some();
        let mut model = Self {
            reaches_devices,
            sections,
            sounds,
            sound,
            runtime: Arc::new(RealAudioRuntime),
            runtime_caps,
            sound_themes,
            event_sounds_available,
            volume_error: error.clone(),
            input_error: error.clone(),
            devices_error: error,
            runtime_notice: None,
            channel_test: None,
            mic_session: None,
            mic_meter: level_meter::Level::default(),
            volume: 0.0,
            input_volume: 0.0,
            muted: false,
            changed: false,
            changes: DeviceChanges::default(),
            output_meter: level_meter::Level::default(),
            metering: std::rc::Weak::new(),
            page: std::rc::Weak::new(),
            sets: SpeakerSets::default(),
            speakers: Arc::new(crate::speaker_setup::BrokerSpeakers),
        };
        model.take_levels();
        model
    }

    fn detached(sections: crate::sections::Sections) -> Self {
        Self {
            reaches_devices: false,
            sections,
            sounds: None,
            sound: Sound::default(),
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
            output_meter: level_meter::Level::default(),
            metering: std::rc::Weak::new(),
            page: std::rc::Weak::new(),
            sets: SpeakerSets::default(),
            speakers: Arc::new(NoSpeakerSets),
        }
    }

    fn sample(sections: crate::sections::Sections) -> Self {
        let sound = Sound {
            output: Some(51),
            volume: Some(0.65),
            muted: false,
            input: Some(52),
            input_volume: Some(0.7),
            input_muted: false,
            outputs: vec![
                device(51, "Speakers", true),
                device(91, "Headphones", false),
            ],
            inputs: vec![device(52, "Built-in Microphone", true)],
            streams: vec![
                stream(84, "Music", "Nightcall", Some(91), true, 0.85),
                stream(90, "Browser", "Conference call", Some(51), true, 0.60),
                stream(95, "Meeting", "Microphone", Some(52), false, 0.75),
            ],
            hardware: Vec::new(),
            ..Sound::default()
        };
        let reading = serde_json::to_string(&sound).unwrap_or_default();
        let mut model = Self::detached(sections);
        model.reaches_devices = true;
        model.sounds = Some(Arc::new(SampleSound(std::sync::Mutex::new(Some(Ok(
            reading,
        ))))));
        model.speakers = Arc::new(SampleSpeakerSets);
        model.take_sound(sound);
        model.output_meter.set(0.46);
        model.output_meter.set_live(true);
        model
    }

    #[cfg(test)]
    #[must_use]
    fn with_sound(mut self, sound: Sound, sounds: Arc<dyn SoundResource>) -> Self {
        self.reaches_devices = true;
        self.sounds = Some(sounds);
        self.take_sound(sound);
        self
    }

    pub fn outputs(&self) -> &[Device] {
        &self.sound.outputs
    }

    pub fn inputs(&self) -> &[Device] {
        &self.sound.inputs
    }

    pub fn streams(&self, playback: bool) -> Vec<&Stream> {
        self.sound
            .streams
            .iter()
            .filter(|stream| stream.playback == playback)
            .collect()
    }

    fn output_device(&self) -> Option<&Device> {
        self.sound
            .outputs
            .iter()
            .find(|device| Some(device.node) == self.sound.output)
    }

    fn input_device(&self) -> Option<&Device> {
        self.sound
            .inputs
            .iter()
            .find(|device| Some(device.node) == self.sound.input)
    }

    #[cfg(test)]
    fn apply(&mut self, effect: &AudioEffect) -> Result<String, String> {
        match self.plan(effect)? {
            Plan::Local => self.apply_local(effect),
            Plan::Sound { action, settle, .. } => {
                let result = self.invoking(action)();
                self.settle(settle, result)
            }
            Plan::Change { work, settle, .. } => {
                let result = work();
                self.settle(settle, result)
            }
        }
    }

    fn invoking(&self, action: SoundAction) -> ChangeWork {
        let sounds = self.sounds.clone();
        Box::new(move || match sounds {
            Some(sounds) => {
                let id = action.id();
                sounds.invoke(id, &action.arguments()).inspect_err(|error| {
                    warn!(target: "tilo::settings", op = id, error = %error, "the sound resource refused a change");
                })
            }
            None => Ok(()),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per device effect, each naming its action and what it settles"
    )]
    fn plan(&mut self, effect: &AudioEffect) -> Result<Plan, String> {
        self.changed = true;
        let sound = |key: ChangeKey, action: SoundAction, settle: Settle| -> Result<Plan, String> {
            Ok(Plan::Sound {
                key,
                action,
                settle,
            })
        };
        match effect {
            AudioEffect::SetOutputVolume { volume } => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a level from 0 to 1 fits a f32"
                )]
                let level = volume.clamp(0.0, 1.0) as f32;
                sound(
                    ChangeKey::OutputVolume,
                    SoundAction::SetVolume {
                        level: f64::from(level),
                    },
                    Settle::OutputVolume(level),
                )
            }
            AudioEffect::SetOutputMuted { muted } => sound(
                ChangeKey::OutputMuted,
                SoundAction::SetMuted { muted: *muted },
                Settle::OutputMuted(*muted),
            ),
            AudioEffect::SetInputVolume { volume } => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a level from 0 to 1 fits a f32"
                )]
                let level = volume.clamp(0.0, 1.0) as f32;
                sound(
                    ChangeKey::InputVolume,
                    SoundAction::SetInputVolume {
                        level: f64::from(level),
                    },
                    Settle::InputVolume(level),
                )
            }
            AudioEffect::SelectOutput { device: name } => {
                let node = self
                    .outputs()
                    .iter()
                    .find(|device| &device.label == name)
                    .map(|device| device.node)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                // Leaving a speaker set: the chosen output first, so the
                // set's sink going away leaves this choice in place.
                let leaving = self
                    .sets
                    .shown
                    .iter()
                    .any(|set| set.output_here)
                    .then(|| Arc::clone(&self.speakers));
                let settle = Settle::Output {
                    id: node,
                    name: name.clone(),
                };
                match leaving {
                    None => sound(
                        ChangeKey::OutputDevice,
                        SoundAction::RouteOutput { node },
                        settle,
                    ),
                    Some(speakers) => {
                        let route = self.invoking(SoundAction::RouteOutput { node });
                        Ok(Plan::Change {
                            key: ChangeKey::OutputDevice,
                            work: Box::new(move || {
                                route()?;
                                speaker_call(
                                    speakers.as_ref(),
                                    ui::collaboration::SpeakerCommand::StopUsingAsOutput,
                                )
                            }),
                            settle,
                        })
                    }
                }
            }
            AudioEffect::SelectSpeakerSet { group } => {
                let set = self
                    .sets
                    .shown
                    .iter()
                    .find(|set| &set.id == group)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                let (speakers, id, group) =
                    (Arc::clone(&self.speakers), set.id.clone(), group.clone());
                Ok(Plan::Change {
                    key: ChangeKey::OutputDevice,
                    work: Box::new(move || {
                        speaker_call(
                            speakers.as_ref(),
                            ui::collaboration::SpeakerCommand::UseAsOutput { group: id },
                        )
                    }),
                    settle: Settle::SpeakerSet(group),
                })
            }
            AudioEffect::SelectInput { device: name } => {
                let node = self
                    .inputs()
                    .iter()
                    .find(|device| &device.label == name)
                    .map(|device| device.node)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                sound(
                    ChangeKey::InputDevice,
                    SoundAction::SelectInput { node },
                    Settle::Input {
                        id: node,
                        name: name.clone(),
                    },
                )
            }
            AudioEffect::SetDeviceProfile {
                device: card,
                profile,
            } => {
                let index = self
                    .sound
                    .hardware
                    .iter()
                    .find(|known| known.node == *card)
                    .and_then(|known| {
                        known
                            .profiles
                            .iter()
                            .find(|known| known.available && &known.label == profile)
                    })
                    .map(|known| known.index)
                    .ok_or_else(|| t("sound-notice-device-gone"))?;
                sound(
                    ChangeKey::Profile(*card),
                    SoundAction::SetProfile {
                        device: *card,
                        profile: index,
                    },
                    Settle::Profile,
                )
            }
            AudioEffect::RouteStream {
                stream,
                device: target,
            } => {
                let known = self.stream(*stream)?;
                // The device' node, as the reading lists it; "follow the
                // default" clears the stream's target instead.
                let node = if is_follow_default(target) {
                    None
                } else {
                    let devices = if known.playback {
                        self.outputs()
                    } else {
                        self.inputs()
                    };
                    Some(
                        devices
                            .iter()
                            .find(|device| &device.label == target)
                            .map(|device| device.node)
                            .ok_or_else(|| t("sound-notice-device-gone"))?,
                    )
                };
                Ok(Plan::Sound {
                    key: ChangeKey::Route(*stream),
                    action: SoundAction::RouteStream {
                        stream: *stream,
                        device: node,
                    },
                    settle: Settle::Route,
                })
            }
            AudioEffect::SetStreamVolume { stream, volume } => {
                self.stream(*stream)?;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a level from 0 to 1 fits a f32"
                )]
                let level = volume.clamp(0.0, 1.0) as f32;
                sound(
                    ChangeKey::StreamVolume(*stream),
                    SoundAction::SetStreamVolume {
                        stream: *stream,
                        level: f64::from(level),
                    },
                    Settle::StreamVolume,
                )
            }
            AudioEffect::SetStreamMuted { stream, muted } => {
                self.stream(*stream)?;
                sound(
                    ChangeKey::StreamMuted(*stream),
                    SoundAction::SetStreamMuted {
                        stream: *stream,
                        muted: *muted,
                    },
                    Settle::StreamMuted,
                )
            }
            _ => Ok(Plan::Local),
        }
    }

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
                for device in &mut self.sound.outputs {
                    device.chosen = device.node == id;
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
                for device in &mut self.sound.outputs {
                    device.chosen = false;
                }
                for set in &mut self.sets.shown {
                    set.output_here = set.id == group;
                }
                self.sets.due = true;
                Ok(t("sound-notice-out-device"))
            }
            Settle::Input { id, name } => {
                result.map_err(|error| self.failed("sound-notice-in-device-err", error))?;
                for device in &mut self.sound.inputs {
                    device.chosen = device.node == id;
                }
                self.remember(AudioDevicePrefs::REMEMBER_INPUT.with(name))?;
                Ok(t("sound-notice-in-device"))
            }
            Settle::Profile => {
                result.map_err(|error| self.failed("sound-notice-profile-err", error))?;
                Ok(t("sound-notice-profile"))
            }
            Settle::Route => {
                result.map_err(|error| {
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
                    .output_device()
                    .filter(|device| device.channels == 2)
                    .ok_or_else(|| t("sound-notice-test-need"))
                    .and_then(|device| self.runtime.start_channel_test(&device.name, left))
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
                    .input_device()
                    .ok_or_else(|| t("sound-notice-mic-none"))
                    .and_then(|device| {
                        self.runtime
                            .start_microphone_meter(&device.name, Duration::from_secs(8))
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

    fn stream(&self, node: u32) -> Result<&Stream, String> {
        self.sound
            .streams
            .iter()
            .find(|stream| stream.node == node)
            .ok_or_else(|| t("sound-notice-stream-gone"))
    }

    fn failed(&mut self, key: &str, error: String) -> String {
        warn!(target: "tilo::settings", op = key, error = %error, "audio change failed");
        let notice = t_args(key, &[("error", error.into())]);
        self.runtime_notice = Some(notice.clone());
        notice
    }

    fn remember(&self, invoke: ui::tilo_resource::Invoke<AudioDevicePrefs>) -> Result<(), String> {
        self.sections
            .invoke(invoke)
            .map_err(|error| t_args("sound-notice-pref-err", &[("error", error.into())]))
    }

    /// The picker label for a stream's *current* route: the device it is
    /// linked to, or "Default" when that device is the system default (so a
    /// stream on the default reads as following it, not pinned).
    fn stream_target_label(&self, stream: &Stream) -> String {
        let devices = if stream.playback {
            self.outputs()
        } else {
            self.inputs()
        };
        match stream
            .target
            .and_then(|node| devices.iter().find(|device| device.node == node))
        {
            Some(device) if device.chosen => follow_shown(),
            Some(device) => device.label.clone(),
            None => follow_shown(),
        }
    }

    fn take_reading(&mut self) -> bool {
        let Some(sounds) = self.sounds.as_deref() else {
            return false;
        };
        let Some(reading) = sounds.latest() else {
            return false;
        };
        self.changed = true;
        match reading.and_then(|reading| reading_of(&reading)) {
            Ok(sound) => {
                self.volume_error = None;
                self.input_error = None;
                self.devices_error = None;
                self.take_sound(sound);
            }
            Err(error) => {
                warn!(target: "tilo::settings", %error, "the sound reading was not understood");
                self.volume_error = Some(error.clone());
                self.input_error = Some(error.clone());
                self.devices_error = Some(error);
            }
        }
        true
    }

    fn take_sound(&mut self, sound: Sound) {
        let moved = self.sound.output != sound.output
            || self.sound.input != sound.input
            || self.sound.outputs.len() != sound.outputs.len()
            || self.sound.inputs.len() != sound.inputs.len()
            || self.sound.streams.len() != sound.streams.len()
            || !self
                .sound
                .outputs
                .iter()
                .map(|device| device.node)
                .eq(sound.outputs.iter().map(|device| device.node))
            || !self
                .sound
                .inputs
                .iter()
                .map(|device| device.node)
                .eq(sound.inputs.iter().map(|device| device.node))
            || !self
                .sound
                .streams
                .iter()
                .map(|stream| stream.node)
                .eq(sound.streams.iter().map(|stream| stream.node));
        let problem = sound.problem.clone();
        self.sound = sound;
        if moved {
            self.channel_test = None;
            self.mic_session = None;
            self.mic_meter.set_live(false);
            self.sets.due = true;
        }
        if let Some(problem) = problem {
            self.runtime_notice = Some(problem);
        }
        self.take_levels();
    }

    fn take_levels(&mut self) {
        self.volume = self.sound.volume.unwrap_or(0.0);
        self.input_volume = self.sound.input_volume.unwrap_or(0.0);
        self.muted = self.sound.muted;
    }

    fn poll_speaker_sets(&mut self) -> bool {
        self.sets.poll(&self.speakers)
    }

    fn tick(&mut self) {
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
        self.take_reading();
        if self.page.upgrade().is_some() && self.poll_speaker_sets() {
            self.changed = true;
        }
        self.follow_output_meter();
    }

    fn follow_output_meter(&self) {
        if !self.reaches_devices {
            return;
        }
        let Some(metering) = self.metering.upgrade() else {
            self.output_meter.set_live(false);
            return;
        };
        let sink = self
            .output_device()
            .map(|device| device.name.clone())
            .filter(|name| !name.is_empty())
            .filter(|_| self.runtime_caps.microphone_meter);
        let mut session = metering.session.borrow_mut();
        if let Some((name, running)) = session.as_mut() {
            if Some(&*name) == sink.as_ref() && running.running() {
                return;
            }
            if running.running() {
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
    fn poll(&mut self) -> bool {
        self.tick();
        std::mem::take(&mut self.changed)
    }

    fn busy(&self) -> bool {
        self.mic_session.is_some() || self.channel_test.is_some()
    }

    /// Start following what a page that shows the model needs: the output's
    /// meter runs while a page holds what this answers. Pages showing the
    /// model share one, so building a page again never restarts it.
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
        self.follow_output_meter();
        Rc::new((page, metering))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_audio_service::sound::{Card, Profile};

    type App = ui::framework::testing::TestApp<crate::SettingsShell>;

    fn change_model(app: &mut App, change: impl FnOnce(&mut AudioModel) + 'static) {
        app.with_widget::<crate::window::Inspected, _>(move |settings| {
            let mut model = area(settings).unwrap().model().borrow_mut();
            change(&mut model);
            model.changed = true;
        })
        .expect("the Settings window is mounted");
    }

    fn frame(app: &mut App) -> String {
        for _ in 0..4 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        app.harness().inspection_json().unwrap()
    }

    fn on_sound() -> App {
        ui::i18n::I18nManager::init("en-US");
        let mut app = App::new().unwrap();
        app.harness().resize((1200, 6000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Sound);
        app
    }

    #[derive(Default)]
    struct RecordingSound {
        published: Mutex<Option<Result<String, String>>>,
        taken: Mutex<Vec<(String, serde_json::Value)>>,
        wake: Mutex<Option<ui::WakeHandle>>,
    }

    impl RecordingSound {
        fn publish(&self, sound: &Sound) {
            let reading = serde_json::to_string(sound).expect("a reading is JSON");
            *self.published.lock().unwrap() = Some(Ok(reading));
            if let Some(wake) = self.wake.lock().unwrap().as_ref() {
                wake.wake();
            }
        }

        fn publish_unreadable(&self) {
            *self.published.lock().unwrap() = Some(Ok("{ not a reading".into()));
        }

        fn taken(&self) -> Vec<(String, serde_json::Value)> {
            self.taken.lock().unwrap().clone()
        }

        fn arguments(&self, action: &str) -> serde_json::Value {
            let taken = self.taken();
            taken.iter().find(|(id, _)| id == action).map_or_else(
                || panic!("{action} was not taken: {taken:?}"),
                |(_, arguments)| arguments.clone(),
            )
        }
    }

    impl SoundResource for RecordingSound {
        fn latest(&self) -> Option<Result<String, String>> {
            self.published.lock().unwrap().take()
        }

        fn attach(&self, wake: ui::WakeHandle) {
            *self.wake.lock().unwrap() = Some(wake);
        }

        fn invoke(
            &self,
            action: &str,
            arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<(), String> {
            self.taken.lock().unwrap().push((
                action.to_owned(),
                serde_json::Value::Object(arguments.clone()),
            ));
            Ok(())
        }
    }

    fn published(sounds: Arc<dyn SoundResource>) -> AudioModel {
        let sample = AudioModel::sample(crate::sections::Sections::default());
        let reading = sample.sound.clone();
        let mut model = sample.with_sound(reading, sounds);
        model.speakers = Arc::new(SampleSpeakerSets);
        model
    }

    /// A page open on the sound is woken by each reading the session
    /// publishes, so a headset plugged in shows without looking for it.
    #[test]
    fn a_new_reading_wakes_the_page_showing_it() {
        let scope = ui::WakeScope::new();
        let _entry = scope.enter();
        let sounds = Arc::new(RecordingSound::default());
        let mut model = AudioModel::from_read(
            crate::sections::Sections::default(),
            AudioRead {
                sounds: Some(Arc::clone(&sounds) as Arc<dyn SoundResource>),
                sound: Sound::default(),
                error: None,
                runtime_caps: RuntimeCapabilities::default(),
                sound_themes: Vec::new(),
                event_sounds_available: false,
            },
        );
        assert!(
            !scope.is_pending(),
            "opening the page asks for no frame of its own"
        );

        let reading = Sound {
            output: Some(91),
            outputs: vec![
                device(51, "Speakers", false),
                device(91, "Headphones", true),
            ],
            ..Sound::default()
        };
        sounds.publish(&reading);
        assert!(scope.is_pending(), "the reading woke the page showing it");
        assert!(model.poll(), "the page shows the reading it was woken for");
        assert_eq!(
            model.output_device().map(|device| device.label.as_str()),
            Some("Headphones"),
        );
    }

    #[test]
    fn the_page_shows_the_published_reading() {
        let sounds = Arc::new(RecordingSound::default());
        let mut app = on_sound();
        {
            let sounds = Arc::clone(&sounds) as Arc<dyn SoundResource>;
            change_model(&mut app, move |model| *model = published(sounds));
        }
        let text = frame(&mut app);
        assert!(text.contains("Speakers"), "{text}");
        assert!(text.contains("Music"), "an app's stream is listed: {text}");
        assert!(text.contains("Browser"), "{text}");
        assert!(
            text.contains("Meeting"),
            "a recording stream is too: {text}"
        );

        let mut reading = AudioModel::sample(crate::sections::Sections::default()).sound;
        reading.output = Some(91);
        reading.outputs = vec![
            device(51, "Speakers", false),
            device(91, "Headphones", true),
        ];
        reading.streams = vec![stream(84, "Music", "Nightcall", Some(91), true, 0.85)];
        sounds.publish(&reading);
        let text = frame(&mut app);
        assert!(
            !text.contains("Browser"),
            "an app that stopped playing is gone: {text}"
        );
        let output = app
            .with_widget::<crate::window::Inspected, _>(|settings| {
                area(settings)
                    .unwrap()
                    .model()
                    .borrow()
                    .output_device()
                    .map(|device| device.label.clone())
            })
            .unwrap();
        assert_eq!(output.as_deref(), Some("Headphones"));
    }

    #[test]
    fn a_change_takes_the_resources_action() {
        let sounds = Arc::new(RecordingSound::default());
        let mut model = published(Arc::clone(&sounds) as Arc<dyn SoundResource>);
        model.sound.hardware.push(Card {
            node: 7,
            label: "Headset".into(),
            profile: Some(1),
            profiles: vec![
                Profile {
                    index: 1,
                    label: "High Fidelity".into(),
                    available: true,
                },
                Profile {
                    index: 2,
                    label: "Headset microphone".into(),
                    available: true,
                },
                Profile {
                    index: 3,
                    label: "Off".into(),
                    available: false,
                },
            ],
        });

        for effect in [
            AudioEffect::SetOutputVolume { volume: 0.30 },
            AudioEffect::SetOutputMuted { muted: true },
            AudioEffect::SetInputVolume { volume: 0.40 },
            AudioEffect::SetDeviceProfile {
                device: 7,
                profile: "Headset microphone".into(),
            },
            AudioEffect::RouteStream {
                stream: 84,
                device: "Speakers".into(),
            },
            AudioEffect::RouteStream {
                stream: 84,
                device: FOLLOW_DEFAULT.into(),
            },
            AudioEffect::SetStreamVolume {
                stream: 90,
                volume: 0.30,
            },
            AudioEffect::SetStreamMuted {
                stream: 95,
                muted: true,
            },
        ] {
            model.apply(&effect).unwrap();
        }

        let level = |arguments: serde_json::Value| {
            arguments["level"].as_f64().expect("a level is a number")
        };
        assert!((level(sounds.arguments("set-volume")) - 0.30).abs() < 0.001);
        assert_eq!(sounds.arguments("set-muted")["muted"], true);
        assert!((level(sounds.arguments("set-input-volume")) - 0.40).abs() < 0.001);
        let profile = sounds.arguments("set-profile");
        assert_eq!(
            (profile["device"].clone(), profile["profile"].clone()),
            (7.into(), 2.into())
        );
        assert_eq!(
            sounds
                .taken()
                .iter()
                .filter(|(id, _)| id == "route-stream")
                .map(|(_, arguments)| (arguments["stream"].clone(), arguments["device"].clone()))
                .collect::<Vec<_>>(),
            vec![(84.into(), 51.into()), (84.into(), serde_json::Value::Null)],
            "a stream goes to the device it was given, or follows the default"
        );
        let volume = sounds.arguments("set-stream-volume");
        assert_eq!(volume["stream"], 90);
        assert_eq!(sounds.arguments("set-stream-muted")["muted"], true);
        assert!((model.volume - 0.30).abs() < 0.001);
        assert!(model.muted);
        assert!((model.input_volume - 0.40).abs() < 0.001);

        assert!(
            model
                .apply(&AudioEffect::SelectOutput {
                    device: "Not here".into()
                })
                .is_err()
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

    #[test]
    fn choosing_a_device_routes_it_and_remembers_it() {
        let sounds = Arc::new(RecordingSound::default());
        let mut app = on_sound();
        {
            let sounds = Arc::clone(&sounds) as Arc<dyn SoundResource>;
            change_model(&mut app, move |model| {
                let published = published(sounds);
                model.sounds = published.sounds;
                model.reaches_devices = true;
                model.sound = published.sound;
                model.sound.inputs.push(device(54, "USB Microphone", false));
            });
        }
        app.effect(&SettingsEffect::from(AudioEffect::SelectOutput {
            device: "Headphones".into(),
        }))
        .unwrap();
        app.effect(&SettingsEffect::from(AudioEffect::SelectInput {
            device: "USB Microphone".into(),
        }))
        .unwrap();
        assert_eq!(sounds.arguments("route-output")["node"], 91);
        assert_eq!(sounds.arguments("select-input")["node"], 54);
        let _ = frame(&mut app);
        let remembered = app.settings::<AudioDevicePrefs>().get();
        assert_eq!(remembered.output_device.as_deref(), Some("Headphones"));
        assert_eq!(remembered.input_device.as_deref(), Some("USB Microphone"));
        assert!(
            app.effect(&SettingsEffect::from(AudioEffect::SelectOutput {
                device: "Not here".into()
            }))
            .is_err()
        );
    }

    #[test]
    fn a_stream_names_the_device_it_plays_on() {
        let model = published(Arc::new(RecordingSound::default()));
        let label = |node: u32| {
            let stream = model.stream(node).expect("the reading lists it");
            model.stream_target_label(stream)
        };
        assert_eq!(label(90), follow_shown(), "on the output in use");
        assert_eq!(label(84), "Headphones", "linked away from it");
        assert_eq!(
            label(95),
            follow_shown(),
            "a recording stream on the input in use"
        );
    }

    #[test]
    fn a_remembered_device_is_put_in_use_again() {
        let mut model = published(Arc::new(RecordingSound::default()));
        let remembered = AudioDevicePrefs {
            output_device: Some("Headphones".into()),
            input_device: Some("USB Microphone".into()),
        };
        model.sound.inputs.push(device(54, "USB Microphone", false));
        let sounds = RecordingSound::default();
        restore_choice(&sounds, &model.sound, &remembered);
        let taken = sounds.taken();
        assert_eq!(taken[0].0, "route-output");
        assert_eq!(taken[0].1["node"], 91);
        assert_eq!(taken[1].0, "select-input");
        assert_eq!(taken[1].1["node"], 54);

        let chosen = AudioDevicePrefs {
            output_device: Some("Speakers".into()),
            ..AudioDevicePrefs::default()
        };
        let sounds = RecordingSound::default();
        restore_choice(&sounds, &model.sound, &chosen);
        assert!(sounds.taken().is_empty());
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

    #[test]
    fn a_test_captures_the_output_the_reading_names() {
        let runtime = Arc::new(RecordingRuntime {
            calls: Mutex::new(Vec::new()),
            drops: Arc::new(AtomicUsize::new(0)),
        });
        let mut model = published(Arc::new(RecordingSound::default()));
        model.runtime = runtime.clone();
        model
            .apply(&AudioEffect::TestSpeaker {
                channel: "left".into(),
            })
            .unwrap();
        model.apply(&AudioEffect::ToggleMicrophoneTest).unwrap();
        assert_eq!(
            runtime.calls.lock().unwrap().as_slice(),
            ["test(speakers,true)", "meter(built-in-microphone)"]
        );
    }

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
        let devices = "Can't read the sound: wpctl: Could not connect to PipeWire";
        assert!(text.contains(devices), "{text}");
        assert!(!text.contains(volume), "{text}");
        assert!(
            !text.contains(&t("sound-apps-empty")),
            "no list of apps that only looks silent: {text}"
        );
    }

    #[test]
    fn a_reading_that_is_not_a_sound_is_an_error_until_the_next() {
        let sounds = Arc::new(RecordingSound::default());
        let mut model = published(Arc::clone(&sounds) as Arc<dyn SoundResource>);
        sounds.publish_unreadable();
        assert!(model.poll());
        assert!(model.volume_error.is_some());
        assert!(model.devices_error.is_some());

        let reading = AudioModel::sample(crate::sections::Sections::default()).sound;
        sounds.publish(&reading);
        assert!(model.poll());
        assert!(model.volume_error.is_none());
        assert!(model.devices_error.is_none());
        assert!(model.input_error.is_none());
    }

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

    #[test]
    fn a_speaker_set_is_an_output_chosen_like_any_other() {
        use ui::collaboration::SpeakerCommand;
        let speakers = Arc::new(RecordingSpeakers::default());
        let sounds = Arc::new(RecordingSound::default());
        let mut app = on_sound();
        {
            let speakers = Arc::clone(&speakers);
            let sounds = Arc::clone(&sounds) as Arc<dyn SoundResource>;
            change_model(&mut app, move |model| {
                let published = published(sounds);
                model.sounds = published.sounds;
                model.reaches_devices = true;
                model.speakers = speakers;
                model.sets = SpeakerSets::default();
            });
        }
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness
                        .inspection_json()
                        .is_ok_and(|frame| frame.contains("Living room"))
                }),
            "the set is in the output list"
        );

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
                .outputs()
                .iter()
                .all(|device| !device.chosen)),
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
        assert_eq!(sounds.arguments("route-output")["node"], 51);
        assert!(!model(&mut app, |model| model.sets.shown[0].output_here));
        assert!(model(&mut app, |model| model
            .outputs()
            .iter()
            .any(|device| device.chosen)));
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

    struct StalledSound {
        gate: Arc<crate::stall::Stall>,
        published: Mutex<Option<Result<String, String>>>,
    }

    impl SoundResource for StalledSound {
        fn latest(&self) -> Option<Result<String, String>> {
            self.published.lock().unwrap().take()
        }

        fn attach(&self, _: ui::WakeHandle) {}

        fn invoke(
            &self,
            _: &str,
            _: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<(), String> {
            if self.gate.wait() {
                Ok(())
            } else {
                Err("the session never answered".into())
            }
        }
    }

    #[test]
    fn a_stalled_session_never_holds_the_window() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let gate = crate::stall::Stall::new();
        {
            let stalled = Arc::clone(&gate);
            change_model(&mut app, move |model| {
                model.sounds = Some(Arc::new(StalledSound {
                    gate: stalled,
                    published: Mutex::new(None),
                }));
                model.volume = 0.65;
            });
        }

        app.start_effect(&SettingsEffect::from(AudioEffect::SetOutputVolume {
            volume: 0.3,
        }))
        .expect("the volume change runs as a job");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("the click");
        let shows_set_level = |app: &mut App| {
            app.with_widget::<crate::window::Inspected, _>(|settings| {
                (area(settings).unwrap().model().borrow().volume - 0.3).abs() < 0.01
            })
            .unwrap()
        };
        assert!(
            !shows_set_level(&mut app),
            "the level is not shown as set before the session took it"
        );

        gate.release();
        let deadline = tilo_test_env::hang_deadline();
        while !shows_set_level(&mut app) {
            assert!(
                std::time::Instant::now() < deadline,
                "the level the session took is shown"
            );
            app.settle();
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_replaced_change_is_dropped() {
        let changes = DeviceChanges::default();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let job = |level: u32| -> ChangeWork {
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
        assert_eq!(first(), None, "the older level never reaches the device");
        assert_eq!(mute(), Some(Ok(())), "another kind of change is kept");
        assert_eq!(ran.lock().unwrap().as_slice(), [80, 0]);
    }
}
