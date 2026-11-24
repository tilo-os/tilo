//! The sound service over the graph `PipeWire` runs.

use std::sync::Arc;

use media::audio::{AudioDevice, AudioGraph, GraphControl};
use media::audio_runtime::BoundedSession;
use serde_json::{Map, Value};
use tilo_actions::{ApiError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, reading};

use super::{
    Card, Device, Profile, SOUND, Sound, SoundAction, SoundTheme, Stream, TestSide, Use, UseKind,
    VOLUME_STEP, resource,
};

impl From<&AudioDevice> for Device {
    fn from(device: &AudioDevice) -> Self {
        Self {
            node: device.id,
            name: device.name.clone(),
            label: device.description.clone(),
            chosen: device.is_default,
            volume: device.volume,
            muted: device.muted.unwrap_or(false),
            channels: device.channels,
            codec: device.codec.clone(),
        }
    }
}

impl From<&media::audio::capture::MediaUse> for Use {
    fn from(use_: &media::audio::capture::MediaUse) -> Self {
        Self {
            client_id: use_.client_id,
            pid: use_.pid,
            app_id: use_.app_id.clone(),
            label: use_.label.clone(),
            binary: use_.binary.to_string_lossy().into_owned(),
            kind: match use_.kind {
                media::audio::capture::MediaKind::Microphone => UseKind::Microphone,
                media::audio::capture::MediaKind::Camera => UseKind::Camera,
            },
        }
    }
}

impl From<&media::audio::AudioStream> for Stream {
    fn from(stream: &media::audio::AudioStream) -> Self {
        Self {
            node: stream.id,
            application: stream.app.clone(),
            media: stream.media_name.clone(),
            playback: stream.playback,
            target: stream.target_id,
            volume: stream.volume,
            muted: stream.muted.unwrap_or(false),
        }
    }
}

impl From<&media::audio::AudioHardwareDevice> for Card {
    fn from(card: &media::audio::AudioHardwareDevice) -> Self {
        Self {
            node: card.id,
            label: card.description.clone(),
            profile: card.active_profile,
            profiles: card
                .profiles
                .iter()
                .map(|profile| Profile {
                    index: profile.index,
                    label: profile.description.clone(),
                    available: profile.available,
                })
                .collect(),
        }
    }
}

impl Sound {
    /// The reading of `graph`, and of who is using the camera or the
    /// microphone in it.
    #[must_use]
    pub fn of(graph: &AudioGraph, uses: &[media::audio::capture::MediaUse]) -> Self {
        let output = graph.default_sink();
        let input = graph.default_source();
        Self {
            output: output.map(|device| device.id),
            volume: output.and_then(|device| device.volume),
            muted: output.and_then(|device| device.muted).unwrap_or(false),
            input: input.map(|device| device.id),
            input_volume: input.and_then(|device| device.volume),
            input_muted: input.and_then(|device| device.muted).unwrap_or(false),
            // A speaker group's sink is where its set plays, not an output
            // of its own: the speaker groups list the set.
            outputs: graph
                .sinks
                .iter()
                .filter(|device| device.name != media::audio::SPEAKER_GROUP_SINK)
                .map(Device::from)
                .collect(),
            inputs: graph.sources.iter().map(Device::from).collect(),
            streams: graph
                .playback_streams
                .iter()
                .chain(&graph.record_streams)
                .map(Stream::from)
                .collect(),
            uses: uses.iter().map(Use::from).collect(),
            // The themes installed are not part of the graph: the service
            // reads them once and puts them in every reading it lands.
            themes: Vec::new(),
            hardware: graph.hardware.iter().map(Card::from).collect(),
            // What this device's sound makes possible, read from the graph
            // the service already keeps: a stereo output to mix down, and a
            // microphone beside the cancelling config. A view asks the
            // service instead of the machine.
            mono_output: output.is_some_and(|device| device.channels >= 2),
            echo_cancellation: echo_cancellation_possible(graph),
            pending: Vec::new(),
            problem: None,
        }
    }
}

/// Whether the device can record with echo cancellation: the cancelling
/// config is installed and there is a microphone to record with.
#[cfg(unix)]
fn echo_cancellation_possible(graph: &AudioGraph) -> bool {
    media::audio_runtime::echo_cancel_config_installed() && !graph.sources.is_empty()
}

#[cfg(not(unix))]
fn echo_cancellation_possible(_: &AudioGraph) -> bool {
    false
}

/// One read of the audio graph: the graph, and who is using the camera or
/// the microphone in it.
#[derive(Clone, Debug, Default)]
pub struct Read {
    pub graph: AudioGraph,
    pub uses: Vec<media::audio::capture::MediaUse>,
}

/// The audio graph under the service.
pub trait Graph: Send + Sync {
    /// Read the graph.
    fn read(&self) -> Read;
    /// What changes it.
    fn control(&self) -> &dyn GraphControl;
    /// Play a short test tone through output `node` on `side` and nowhere
    /// else; it stops by itself. A graph that plays none says so, so a test
    /// graph needs no tone of its own.
    ///
    /// # Errors
    /// This graph plays no test tone.
    fn test_tone(&self, _node: u32, _side: TestSide) -> Result<Box<dyn BoundedSession>, String> {
        Err("this sound graph plays no test tone".into())
    }
}

/// The graph `PipeWire` runs.
pub struct SystemGraph(Arc<dyn GraphControl>);

impl Default for SystemGraph {
    fn default() -> Self {
        Self(media::audio::real_control())
    }
}

impl Graph for SystemGraph {
    fn read(&self) -> Read {
        media::audio::try_read()
            .map(|(graph, uses)| Read { graph, uses })
            .unwrap_or_default()
    }

    fn control(&self) -> &dyn GraphControl {
        self.0.as_ref()
    }

    fn test_tone(&self, node: u32, side: TestSide) -> Result<Box<dyn BoundedSession>, String> {
        let read = self.read();
        let output = read
            .graph
            .sink_by_id(node)
            .ok_or_else(|| "that output is gone".to_string())?;
        media::audio_runtime::play_test_tone(&output.name, side.left())
    }
}

/// A change to make on the graph.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Volume {
        level: f32,
        /// Raising the sound out of silence is the person asking to hear
        /// it, so the change unmutes as well.
        unmute: bool,
    },
    Mute(bool),
    Output(u32),
    Input(u32),
    InputMute(bool),
    InputVolume(f32),
    StreamRoute {
        stream: u32,
        /// The output (or input) it plays through; `None` follows the
        /// default again.
        device: Option<u32>,
    },
    StreamVolume {
        stream: u32,
        level: f32,
    },
    StreamMute {
        stream: u32,
        muted: bool,
    },
    Profile {
        device: u32,
        profile: u32,
    },
}

impl Change {
    /// Make it on `graph`, read just before.
    fn make(self, graph: &AudioGraph, control: &dyn GraphControl) -> Result<(), String> {
        let output = || graph.default_sink().ok_or("there is no sound output");
        let input = || graph.default_source().ok_or("there is no sound input");
        let node_of = |node: u32| {
            graph
                .playback_streams
                .iter()
                .chain(&graph.record_streams)
                .find(|stream| stream.id == node)
                .ok_or_else(|| "that application's stream has ended".to_string())
        };
        match self {
            Self::Volume { level, unmute } => {
                let node = output()?;
                control.set_node_volume(node.id, level.clamp(0.0, 1.0))?;
                if unmute && node.muted == Some(true) {
                    control.set_node_mute(node.id, false)?;
                }
                Ok(())
            }
            Self::Mute(muted) => control.set_node_mute(output()?.id, muted),
            Self::InputMute(muted) => control.set_node_mute(input()?.id, muted),
            Self::InputVolume(level) => control.set_node_volume(input()?.id, level.clamp(0.0, 1.0)),
            Self::Output(node) => {
                graph.sink_by_id(node).ok_or("that output is gone")?;
                control.set_default_sink(node)?;
                for stream in &graph.playback_streams {
                    control.unroute_stream(stream.id)?;
                }
                Ok(())
            }
            Self::Input(node) => {
                graph
                    .sources
                    .iter()
                    .find(|source| source.id == node)
                    .ok_or("that input is gone")?;
                control.set_default_source(node)?;
                for stream in &graph.record_streams {
                    control.unroute_stream(stream.id)?;
                }
                Ok(())
            }
            Self::StreamRoute { stream, device } => {
                let known = node_of(stream)?;
                let Some(device) = device else {
                    return control.unroute_stream(known.id);
                };
                let target = if known.playback {
                    graph.sink_by_id(device)
                } else {
                    graph.sources.iter().find(|source| source.id == device)
                };
                control.route_stream(known.id, target.ok_or("that device is gone")?.serial)
            }
            Self::StreamVolume { stream, level } => {
                control.set_node_volume(node_of(stream)?.id, level.clamp(0.0, 1.0))
            }
            Self::StreamMute { stream, muted } => control.set_node_mute(node_of(stream)?.id, muted),
            Self::Profile { device, profile } => control.set_device_profile(device, profile),
        }
    }
}

/// What lands from the worker.
enum Landed {
    Read(Sound),
    Changed {
        action: String,
        result: Result<(), String>,
        sound: Sound,
        /// The test tone now playing, when the action started one.
        tone: Option<Box<dyn BoundedSession>>,
    },
}

/// The sound service.
pub struct SoundService {
    sound: Sound,
    /// The sound themes installed here, which do not change while the
    /// service runs and are published with every reading.
    themes: Vec<SoundTheme>,
    /// The test tones still playing, each stopped by itself.
    tones: Vec<Box<dyn BoundedSession>>,
    worker: Worker<dyn Graph, Landed>,
}

impl SoundService {
    /// The service over `graph`, reading it once at the start.
    ///
    /// # Errors
    /// The worker's thread did not start.
    pub fn new(graph: Arc<dyn Graph>, wake: Option<Wake>) -> std::io::Result<Self> {
        let service = Self {
            sound: Sound::default(),
            themes: installed_themes(),
            tones: Vec::new(),
            worker: Worker::spawn("tilo-audio-sound", graph, wake)?,
        };
        service.refresh();
        Ok(service)
    }

    /// Read the graph again.
    pub fn refresh(&self) {
        refresh(&self.worker.jobs());
    }

    /// What asks for a fresh reading from another thread (the policy's
    /// device events).
    pub fn refresher(&self) -> impl Fn() + Send + 'static {
        let jobs = self.worker.jobs();
        move || refresh(&jobs)
    }

    /// What it reads now.
    #[must_use]
    pub const fn sound(&self) -> &Sound {
        &self.sound
    }

    fn land(&mut self, landed: Landed) {
        let (mut sound, done) = match landed {
            Landed::Read(sound) => (sound, None),
            Landed::Changed {
                action,
                result,
                sound,
                tone,
            } => {
                if let Some(tone) = tone {
                    self.tones.push(tone);
                }
                (sound, Some((action, result)))
            }
        };
        sound.pending = std::mem::take(&mut self.sound.pending);
        sound.problem = self.sound.problem.take();
        sound.themes = self.themes.clone();
        if let Some((action, result)) = done {
            if let Some(at) = sound.pending.iter().position(|id| *id == action) {
                sound.pending.remove(at);
            }
            sound.problem = result.err();
        }
        self.sound = sound;
    }

    fn start(&mut self, action: &str, change: Change) -> Started {
        self.sound.pending.push(action.to_owned());
        let action = action.to_owned();
        self.worker.run(move |graph| {
            let before = graph.read();
            let result = change.make(&before.graph, graph.control());
            let after = graph.read();
            Landed::Changed {
                action,
                result,
                sound: Sound::of(&after.graph, &after.uses),
                tone: None,
            }
        });
        Started::Accepted
    }

    /// Play a test tone through output `node`, off the service's own thread
    /// like every other change, and keep it until it stops by itself.
    fn start_tone(&mut self, action: &str, node: u32, side: TestSide) -> Started {
        self.sound.pending.push(action.to_owned());
        let action = action.to_owned();
        self.worker.run(move |graph| {
            let result = graph.test_tone(node, side);
            let (result, tone) = match result {
                Ok(tone) => (Ok(()), Some(tone)),
                Err(error) => (Err(error), None),
            };
            let after = graph.read();
            Landed::Changed {
                action,
                result,
                sound: Sound::of(&after.graph, &after.uses),
                tone,
            }
        });
        Started::Accepted
    }

    /// Forget the test tones that have stopped, stopping the ones that ran
    /// past their time.
    fn reap_tones(&mut self) {
        self.tones.retain_mut(|tone| tone.running());
    }

    /// The level a step from the reading lands on.
    fn stepped(&self, step: f32) -> f32 {
        (self.sound.volume.unwrap_or(0.0) + step).clamp(0.0, 1.0)
    }
}

fn refresh(jobs: &Jobs<dyn Graph, Landed>) {
    jobs.run(|graph| {
        let read = graph.read();
        Landed::Read(Sound::of(&read.graph, &read.uses))
    });
}

/// The sound themes installed here, as the reading names them.
fn installed_themes() -> Vec<SoundTheme> {
    media::event_sound::installed_themes()
        .into_iter()
        .map(|theme| SoundTheme {
            id: theme.id,
            name: theme.name,
        })
        .collect()
}

/// The level a `f64` argument names, from 0 to 1.
fn level_of(level: f64) -> Result<f32, ApiError> {
    if !(0.0..=1.0).contains(&level) {
        return Err(ApiError::InvalidArguments(format!(
            "a level is from 0 to 1, not {level}"
        )));
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a level from 0 to 1 fits a f32"
    )]
    Ok(level as f32)
}

impl Service for SoundService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource(&self.sound)]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == SOUND).then(|| reading(&self.sound))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let action = decode::<SoundAction>(&self.resources(), resource, action, arguments)?;
        let id = action.id();
        let change = match action {
            SoundAction::TestOutput { node, channel } => {
                let side = TestSide::of(channel.as_deref()).map_err(ApiError::InvalidArguments)?;
                return Ok(self.start_tone(id, node, side));
            }
            SoundAction::SetVolume { level } => {
                let level = level_of(level)?;
                // A slider still at or above the level it reads, moved up,
                // is the person turning the sound back on.
                Change::Volume {
                    level,
                    unmute: level > 0.0 && level >= self.sound.volume.unwrap_or(0.0),
                }
            }
            SoundAction::VolumeUp => Change::Volume {
                level: self.stepped(VOLUME_STEP),
                unmute: true,
            },
            SoundAction::VolumeDown => Change::Volume {
                level: self.stepped(-VOLUME_STEP),
                unmute: false,
            },
            SoundAction::SetMuted { muted } => Change::Mute(muted),
            SoundAction::ToggleMuted => Change::Mute(!self.sound.muted),
            SoundAction::RouteOutput { node } => Change::Output(node),
            SoundAction::SelectInput { node } => Change::Input(node),
            SoundAction::SetInputMuted { muted } => Change::InputMute(muted),
            SoundAction::ToggleInputMuted => Change::InputMute(!self.sound.input_muted),
            SoundAction::SetInputVolume { level } => Change::InputVolume(level_of(level)?),
            SoundAction::RouteStream { stream, device } => Change::StreamRoute { stream, device },
            SoundAction::SetStreamVolume { stream, level } => Change::StreamVolume {
                stream,
                level: level_of(level)?,
            },
            SoundAction::SetStreamMuted { stream, muted } => Change::StreamMute { stream, muted },
            SoundAction::SetProfile { device, profile } => Change::Profile { device, profile },
        };
        Ok(self.start(id, change))
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        self.reap_tones();
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
        self.reap_tones();
    }
}

#[cfg(test)]
mod tests;
