//! The sound service over the graph `PipeWire` runs.

use std::sync::Arc;

use media::audio::{AudioDevice, AudioGraph, GraphControl};
use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, reading};

use super::{Device, SOUND, Sound, SoundAction, VOLUME_STEP, resource};

impl From<&AudioDevice> for Device {
    fn from(device: &AudioDevice) -> Self {
        Self {
            node: device.id,
            label: device.description.clone(),
            chosen: device.is_default,
            volume: device.volume,
            muted: device.muted.unwrap_or(false),
        }
    }
}

impl Sound {
    /// The reading of `graph`.
    #[must_use]
    pub fn of(graph: &AudioGraph) -> Self {
        let output = graph.default_sink();
        let input = graph.default_source();
        Self {
            output: output.map(|device| device.id),
            volume: output.and_then(|device| device.volume),
            muted: output.and_then(|device| device.muted).unwrap_or(false),
            input: input.map(|device| device.id),
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
            pending: Vec::new(),
            problem: None,
        }
    }
}

/// The audio graph under the service.
pub trait Graph: Send + Sync {
    /// Read the graph.
    fn read(&self) -> AudioGraph;
    /// What changes it.
    fn control(&self) -> &dyn GraphControl;
}

/// The graph `PipeWire` runs.
pub struct SystemGraph(Arc<dyn GraphControl>);

impl Default for SystemGraph {
    fn default() -> Self {
        Self(media::audio::real_control())
    }
}

impl Graph for SystemGraph {
    fn read(&self) -> AudioGraph {
        media::audio::enumerate()
    }

    fn control(&self) -> &dyn GraphControl {
        self.0.as_ref()
    }
}

/// A change to make on the graph.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Volume(f32),
    Mute(bool),
    Output(u32),
    Input(u32),
    InputMute(bool),
}

impl Change {
    /// Make it on `graph`, read just before.
    fn make(self, graph: &AudioGraph, control: &dyn GraphControl) -> Result<(), String> {
        let output = || graph.default_sink().ok_or("there is no sound output");
        let input = || graph.default_source().ok_or("there is no sound input");
        match self {
            Self::Volume(level) => control.set_node_volume(output()?.id, level.clamp(0.0, 1.0)),
            Self::Mute(muted) => control.set_node_mute(output()?.id, muted),
            Self::InputMute(muted) => control.set_node_mute(input()?.id, muted),
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
    },
}

/// The sound service.
pub struct SoundService {
    sound: Sound,
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
            } => (sound, Some((action, result))),
        };
        sound.pending = std::mem::take(&mut self.sound.pending);
        sound.problem = self.sound.problem.take();
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
            let result = change.make(&before, graph.control());
            Landed::Changed {
                action,
                result,
                sound: Sound::of(&graph.read()),
            }
        });
        Started::Accepted
    }

    /// The level a step from the reading lands on.
    fn stepped(&self, step: f32) -> f32 {
        (self.sound.volume.unwrap_or(0.0) + step).clamp(0.0, 1.0)
    }
}

fn refresh(jobs: &Jobs<dyn Graph, Landed>) {
    jobs.run(|graph| Landed::Read(Sound::of(&graph.read())));
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
    ) -> Result<Started, DoorError> {
        let action = decode::<SoundAction>(&self.resources(), resource, action, arguments)?;
        let id = action.id();
        let change = match action {
            SoundAction::SetVolume { level } => {
                if !(0.0..=1.0).contains(&level) {
                    return Err(DoorError::InvalidArguments(format!(
                        "a volume is from 0 to 1, not {level}"
                    )));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a level from 0 to 1 fits a f32"
                )]
                Change::Volume(level as f32)
            }
            SoundAction::VolumeUp => Change::Volume(self.stepped(VOLUME_STEP)),
            SoundAction::VolumeDown => Change::Volume(self.stepped(-VOLUME_STEP)),
            SoundAction::SetMuted { muted } => Change::Mute(muted),
            SoundAction::ToggleMuted => Change::Mute(!self.sound.muted),
            SoundAction::RouteOutput { node } => Change::Output(node),
            SoundAction::SelectInput { node } => Change::Input(node),
            SoundAction::SetInputMuted { muted } => Change::InputMute(muted),
            SoundAction::ToggleInputMuted => Change::InputMute(!self.sound.input_muted),
        };
        Ok(self.start(id, change))
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        for landed in landed {
            self.land(landed);
        }
        changed
    }

    fn settle(&mut self) {
        for landed in self.worker.settle() {
            self.land(landed);
        }
    }
}

#[cfg(test)]
mod tests;
