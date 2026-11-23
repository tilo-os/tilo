//! A playback's sound on several of the computer's sound devices at once, each at its own
//! volume and its own distance behind the first.
//!
//! [`SinkHost`] is the [`OutputHost`] a [`Playout`](crate::playback::Playout) applies a playback's
//! sink outputs to. It opens one stream per output through a [`SinkFactory`], fans each piece of
//! PCM out to all of them with per-output gain, and holds each output back by the frames its sync
//! asks for, so a device further from the listener waits in a delay line instead of arriving
//! early. A stream that fails is dropped alone and said through [`SinkHost::events`]; the others
//! keep playing.
//!
//! The real factory is [`SystemSinkFactory`]: the devices the `PipeWire`/`WirePlumber` graph lists,
//! each opened on its own node, and an output named [`DEFAULT_SINK`] follows whatever the computer
//! plays through now ([`SinkHost::follow`] reopens it when that changes). A build without the
//! native streams plays the default through the shared [`crate::audio_output`] route every host
//! installs, and can open no other device.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[cfg(all(feature = "pipewire", target_os = "linux"))]
use crate::audio::AudioDevice;
use crate::audio::{self, AudioGraph};
use crate::audio_output;
use crate::playback::{Output, OutputHost, OutputKind, OutputState};

/// The device name an output takes when it follows whatever the computer plays through now.
pub const DEFAULT_SINK: &str = "default";

/// How many sound devices one playback may play on at once. Well past a room's worth of
/// speakers, low enough that a stray repeated add cannot open streams without bound.
pub const MAX_SINKS: usize = 8;

/// The longest an output may wait behind the first, in milliseconds: past this a
/// person is correcting a broken device, not sync.
pub const MAX_SYNC_MS: i64 = 2_000;

#[allow(
    clippy::cast_lossless,
    reason = "a const initializer cannot call the lossless `From` conversion"
)]
const CHANNELS: usize = audio_output::CHANNELS as usize;

/// Something a host did that the playback did not ask for: an output that began, moved state, or
/// stopped being able to play. A playback applies these to its outputs list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEvent {
    /// The output it is about.
    pub id: String,
    pub state: OutputState,
    /// What a person reads when `state` is [`OutputState::Failed`]; for
    /// [`OutputState::Connecting`] it is the stage reached.
    pub detail: Option<String>,
}

/// One sound device as the audio graph names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkDevice {
    /// The `node.name` an output names this device by.
    pub name: String,
    /// What a person reads it as.
    pub label: String,
    pub serial: u32,
    pub is_default: bool,
}

/// One open stream to a sound device. PCM is stereo, 48 kHz, interleaved S16, at most
/// [`audio_output::MAX_SAMPLES`] frames per write.
pub trait SinkStream: Send {
    /// Play `samples` now, with bounded backpressure: a stream that cannot keep up drops sound
    /// rather than hold up the others.
    ///
    /// # Errors
    /// Why this device stopped playing.
    fn write(&mut self, samples: &[i16]) -> Result<(), String>;
    fn close(&mut self);
}

/// One opened stream and the device it is really on: a [`DEFAULT_SINK`] output resolves to the
/// device playing now, which is what [`SinkHost::follow`] compares against the next default.
pub struct OpenSink {
    pub device: String,
    pub stream: Box<dyn SinkStream>,
}

/// Where a [`SinkHost`] gets its streams: the shared route, the real sound devices, or a test's
/// recording one.
pub trait SinkFactory: Send {
    /// Open a stream to `device`: a sound device's `node.name` or its description, or
    /// [`DEFAULT_SINK`] for whatever the computer plays through now.
    ///
    /// # Errors
    /// Why the device could not be opened (gone, no hardware, output silenced).
    fn open(&self, device: &str) -> Result<OpenSink, String>;
}

/// The computer's sound devices, as a person picks them.
///
/// # Errors
/// When the audio graph cannot be read (no `WirePlumber`, or a build without it).
pub fn devices() -> Result<Vec<SinkDevice>, String> {
    Ok(devices_of(&audio::try_enumerate()?))
}

/// The devices `graph` lists as outputs a playback can play on. The private sink a speaker group
/// plays through is not one of them: a group is played as a set, never as a device.
#[must_use]
pub fn devices_of(graph: &AudioGraph) -> Vec<SinkDevice> {
    graph
        .sinks
        .iter()
        .filter(|device| device.name != audio::SPEAKER_GROUP_SINK)
        .map(|device| SinkDevice {
            name: device.name.clone(),
            label: device.description.clone(),
            serial: device.serial,
            is_default: device.is_default,
        })
        .collect()
}

/// A playback's sound outputs, playing.
pub struct SinkHost<F: SinkFactory> {
    factory: F,
    sinks: BTreeMap<String, Sink>,
    events: Vec<HostEvent>,
}

struct Sink {
    /// What the output asked for; [`DEFAULT_SINK`] keeps following the computer's default.
    device: String,
    /// The device the open stream is on now.
    opened: String,
    stream: Box<dyn SinkStream>,
    gain: f32,
    sync_ms: i64,
    /// Frames this output stands behind the leader, and the sound waiting for them.
    delay: usize,
    line: VecDeque<i16>,
}

impl<F: SinkFactory> SinkHost<F> {
    #[must_use]
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            sinks: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// The outputs that changed without being asked since the last call.
    pub fn events(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.events)
    }

    /// The outputs playing, with the device each is on.
    #[must_use]
    pub fn playing(&self) -> Vec<(&str, &str)> {
        self.sinks
            .iter()
            .map(|(id, sink)| (id.as_str(), sink.opened.as_str()))
            .collect()
    }

    /// Hand one piece of the playback's sound to every output, each with its own gain, each as
    /// far behind the first as its sync asks. An output that stops is dropped and said; the
    /// others keep every later piece.
    pub fn write(&mut self, samples: &[i16]) {
        let whole = samples
            .len()
            .checked_div(CHANNELS)
            .unwrap_or(0)
            .saturating_mul(CHANNELS);
        let Some(samples) = samples.get(..whole) else {
            return;
        };
        let mut failed = Vec::new();
        for (id, sink) in &mut self.sinks {
            if let Err(why) = sink.write(samples) {
                failed.push((id.clone(), why));
            }
        }
        for (id, why) in failed {
            self.sinks.remove(&id);
            self.events.push(HostEvent {
                id,
                state: OutputState::Failed,
                detail: Some(why),
            });
        }
    }

    /// Drop the sound waiting in every output's delay line: when the playback jumps somewhere,
    /// each speaker starts from there instead of playing the seconds it stood behind.
    pub fn flush(&mut self) {
        for sink in self.sinks.values_mut() {
            sink.line.clear();
            sink.line.resize(sink.delay.saturating_mul(CHANNELS), 0);
        }
    }

    /// Follow the audio graph: an output on [`DEFAULT_SINK`] moves to the new default, a filter
    /// that arrived on the default path is still the same output, and an output whose device
    /// went away is dropped and said.
    pub fn follow(&mut self, graph: &AudioGraph) {
        let default = graph.default_sink().map(|device| device.name.clone());
        let present: BTreeSet<&str> = graph
            .sinks
            .iter()
            .map(|device| device.name.as_str())
            .collect();
        let mut moves = Vec::new();
        for (id, sink) in &self.sinks {
            if sink.device == DEFAULT_SINK {
                match default.clone() {
                    Some(name) if name != sink.opened => moves.push((id.clone(), Some(name))),
                    Some(_) => {}
                    None => moves.push((id.clone(), None)),
                }
            } else if !present.contains(sink.opened.as_str()) {
                // A named device does not follow anything; it either is still there or it went.
                moves.push((id.clone(), None));
            }
        }
        for (id, device) in moves {
            let Some(device) = device else {
                self.sinks.remove(&id);
                self.events.push(HostEvent {
                    id,
                    state: OutputState::Failed,
                    detail: Some("That speaker went away.".to_owned()),
                });
                continue;
            };
            let opened = self.factory.open(&device);
            let Some(sink) = self.sinks.get_mut(&id) else {
                if let Ok(mut opened) = opened {
                    opened.stream.close();
                }
                continue;
            };
            match opened {
                Ok(opened) => {
                    sink.stream.close();
                    sink.stream = opened.stream;
                    sink.opened = opened.device;
                    sink.line.clear();
                    sink.line.resize(sink.delay.saturating_mul(CHANNELS), 0);
                    self.events.push(HostEvent {
                        id,
                        state: OutputState::Playing,
                        detail: None,
                    });
                }
                Err(why) => {
                    self.sinks.remove(&id);
                    self.events.push(HostEvent {
                        id,
                        state: OutputState::Failed,
                        detail: Some(why),
                    });
                }
            }
        }
    }

    /// Hold each output back by the frames its sync asks for against the earliest one, so the
    /// relative timing a person set is what plays.
    fn realign(&mut self) {
        let Some(earliest) = self.sinks.values().map(|sink| sink.sync_ms).min() else {
            return;
        };
        let rate = i64::from(audio_output::RATE);
        for sink in self.sinks.values_mut() {
            let behind = sink.sync_ms.saturating_sub(earliest).clamp(0, MAX_SYNC_MS);
            let frames =
                usize::try_from(behind.saturating_mul(rate).checked_div(1_000).unwrap_or(0))
                    .unwrap_or(0);
            sink.set_delay(frames);
        }
    }
}

impl<F: SinkFactory> OutputHost for SinkHost<F> {
    fn start(&mut self, output: &Output) -> Result<(), String> {
        let OutputKind::Sink { device } = &output.kind else {
            return Err("That output is not a sound device.".to_owned());
        };
        if self.sinks.contains_key(&output.id) {
            return Err("That output is already playing.".to_owned());
        }
        if self.sinks.len() >= MAX_SINKS {
            return Err(format!(
                "A playback plays on at most {MAX_SINKS} sound devices."
            ));
        }
        let opened = self.factory.open(device)?;
        self.sinks.insert(
            output.id.clone(),
            Sink {
                device: device.clone(),
                opened: opened.device,
                stream: opened.stream,
                gain: output.volume.clamp(0.0, 1.0),
                sync_ms: output.sync_ms,
                delay: 0,
                line: VecDeque::new(),
            },
        );
        self.realign();
        Ok(())
    }

    fn update(&mut self, output: &Output) -> Result<(), String> {
        let OutputKind::Sink { device } = &output.kind else {
            return Err("That output is not a sound device.".to_owned());
        };
        let moved = self
            .sinks
            .get(&output.id)
            .ok_or("That output is not playing.")?
            .device
            != *device;
        if moved {
            // The same output naming another device is that device now: a person moved it.
            let opened = self.factory.open(device)?;
            let sink = self
                .sinks
                .get_mut(&output.id)
                .ok_or("That output is not playing.")?;
            sink.stream.close();
            sink.stream = opened.stream;
            sink.opened = opened.device;
            sink.device.clone_from(device);
            sink.line.clear();
        }
        let sink = self
            .sinks
            .get_mut(&output.id)
            .ok_or("That output is not playing.")?;
        sink.gain = output.volume.clamp(0.0, 1.0);
        sink.sync_ms = output.sync_ms;
        self.realign();
        Ok(())
    }

    fn stop(&mut self, id: &str) {
        if let Some(mut sink) = self.sinks.remove(id) {
            sink.stream.close();
        }
    }
}

impl<F: SinkFactory> Drop for SinkHost<F> {
    /// A playback that ends takes its streams with it: nothing of it keeps playing.
    fn drop(&mut self) {
        for (_, mut sink) in std::mem::take(&mut self.sinks) {
            sink.stream.close();
        }
    }
}

impl Sink {
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        if self.delay == 0 {
            let scaled = scaled(samples, self.gain);
            return self.stream.write(&scaled);
        }
        self.line.extend(samples.iter().copied());
        let wanted = samples.len().min(self.line.len());
        let mut out = Vec::with_capacity(wanted);
        for _ in 0..wanted {
            out.push(self.line.pop_front().unwrap_or(0));
        }
        let scaled = scaled(&out, self.gain);
        self.stream.write(&scaled)
    }

    /// Stand `frames` behind the incoming sound, keeping whatever of it is already waiting.
    fn set_delay(&mut self, frames: usize) {
        let wanted = frames.saturating_mul(CHANNELS);
        while self.line.len() > wanted {
            self.line.pop_front();
        }
        self.line.resize(wanted, 0);
        self.delay = frames;
    }
}

/// `samples` at `gain`: silence at zero, unchanged at one, clipped at the ends of an `i16`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped into the range an i16 holds before the cast"
)]
fn scaled(samples: &[i16], gain: f32) -> Vec<i16> {
    if gain >= 1.0 {
        return samples.to_vec();
    }
    samples
        .iter()
        .map(|sample| {
            let value = f32::from(*sample) * gain;
            value.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        })
        .collect()
}

/// The real sound devices of this computer.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSinkFactory;

impl SinkFactory for SystemSinkFactory {
    fn open(&self, device: &str) -> Result<OpenSink, String> {
        if audio_output::is_disabled() {
            return Err("Sound output is off.".to_owned());
        }
        Self::open_device(device)
    }
}

impl SystemSinkFactory {
    /// The device's stream: a chosen node of the audio graph, or the default through it.
    #[cfg(all(feature = "pipewire", target_os = "linux"))]
    fn open_device(device: &str) -> Result<OpenSink, String> {
        match audio::try_enumerate() {
            Ok(graph) => {
                let target = resolve(&graph, device)?;
                Ok(OpenSink {
                    device: target.name.clone(),
                    stream: open_on_device(Some(target.serial), &target.description)?,
                })
            }
            Err(error) if device == DEFAULT_SINK => {
                // The graph could not be read, but the default device still exists: an
                // untargeted stream is whatever plays now.
                tracing::debug!(%error, "audio graph unreadable; playing on the default device");
                Ok(OpenSink {
                    device: DEFAULT_SINK.to_owned(),
                    stream: open_on_device(None, "Tilo playback")?,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// The shared route every host installs, which opens the default and nothing else.
    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    fn open_device(device: &str) -> Result<OpenSink, String> {
        if device != DEFAULT_SINK {
            return Err("This build plays only on the computer's default sound output.".to_owned());
        }
        let name = audio::try_enumerate()
            .ok()
            .and_then(|graph| graph.default_sink().map(|sink| sink.name.clone()))
            .unwrap_or_else(|| DEFAULT_SINK.to_owned());
        Ok(OpenSink {
            device: name,
            stream: Box::new(DefaultRouteSink {
                output: audio_output::open()?,
            }),
        })
    }
}

/// The device `device` names in `graph`: its `node.name`, its description, or its serial.
#[cfg(all(feature = "pipewire", target_os = "linux"))]
fn resolve<'a>(graph: &'a AudioGraph, device: &str) -> Result<&'a AudioDevice, String> {
    if device == DEFAULT_SINK {
        return graph
            .default_sink()
            .ok_or_else(|| "The computer has no sound output.".to_owned());
    }
    graph
        .sinks
        .iter()
        .find(|candidate| candidate.name == device)
        .or_else(|| {
            graph
                .sinks
                .iter()
                .find(|candidate| candidate.description == device)
        })
        .or_else(|| {
            device
                .parse::<u32>()
                .ok()
                .and_then(|serial| graph.sinks.iter().find(|c| c.serial == serial))
        })
        .ok_or_else(|| format!("{device} is not a sound output of this computer."))
}

/// The default route's stream: the shared one every host installs, which respects the person's
/// lockdown switch on every write.
#[cfg(not(all(feature = "pipewire", target_os = "linux")))]
struct DefaultRouteSink {
    output: Box<dyn audio_output::Output>,
}

#[cfg(not(all(feature = "pipewire", target_os = "linux")))]
impl SinkStream for DefaultRouteSink {
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        // The shared route takes at most one 20 ms block per call.
        let block = audio_output::MAX_SAMPLES.saturating_mul(CHANNELS);
        for packet in samples.chunks(block) {
            self.output.write(packet)?;
        }
        Ok(())
    }

    fn close(&mut self) {
        self.output.close();
    }
}

/// One stream on a chosen device, fed from the fan-out through a short ring bounded to the
/// device's own latency: a device slower than the playback loses the oldest sound instead of
/// making every other output late.
#[cfg(all(feature = "pipewire", target_os = "linux"))]
fn open_on_device(serial: Option<u32>, description: &str) -> Result<Box<dyn SinkStream>, String> {
    use std::sync::{Arc, Mutex};

    use tilo_lock::MutexRecover as _;

    use crate::pw_stream::{self, PlaybackConfig, PlaybackSource};

    /// 100 ms of stereo at the crate's rate (48 kHz): enough for a jittery device, short enough
    /// that a stalled one cannot make the output late.
    const RING_SAMPLES: usize = 9_600;

    #[derive(Default)]
    struct Ring {
        pcm: VecDeque<f32>,
    }

    impl Ring {
        fn push(&mut self, samples: &[i16]) {
            for sample in samples {
                if self.pcm.len() >= RING_SAMPLES {
                    self.pcm.pop_front();
                }
                self.pcm.push_back(f32::from(*sample) / 32_768.0);
            }
        }
    }

    struct Source {
        ring: Arc<Mutex<Ring>>,
    }

    impl PlaybackSource for Source {
        fn render(&mut self, _presentation_ns: i64, out: &mut [f32]) {
            let mut ring = self.ring.lock_recover();
            for slot in out.iter_mut() {
                *slot = ring.pcm.pop_front().unwrap_or(0.0);
            }
        }
    }

    struct DeviceSink {
        stream: Option<Box<dyn pw_stream::PlaybackStream>>,
        ring: Arc<Mutex<Ring>>,
    }

    impl SinkStream for DeviceSink {
        fn write(&mut self, samples: &[i16]) -> Result<(), String> {
            if audio_output::is_disabled() {
                return Err("Sound output is off.".to_owned());
            }
            let stream = self
                .stream
                .as_ref()
                .ok_or_else(|| "The sound output was closed.".to_owned())?;
            if let Some(failure) = stream.failure() {
                return Err(failure);
            }
            self.ring.lock_recover().push(samples);
            Ok(())
        }

        fn close(&mut self) {
            // Dropping the stream stops its thread.
            self.stream = None;
            self.ring.lock_recover().pcm.clear();
        }
    }

    let ring = Arc::new(Mutex::new(Ring::default()));
    let config = PlaybackConfig {
        node_name: "tilo-playback-sink".to_owned(),
        description: description.to_owned(),
        rate: audio_output::RATE,
        channels: audio_output::CHANNELS,
        latency_frames: audio_output::RATE.checked_div(100).unwrap_or(480),
        target_serial: serial,
    };
    let stream = pw_stream::native_streams()
        .open_playback(
            &config,
            Box::new(Source {
                ring: Arc::clone(&ring),
            }),
        )
        .map_err(|error| format!("The sound output would not open: {error}"))?;
    Ok(Box::new(DeviceSink {
        stream: Some(stream),
        ring,
    }))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and their counting cannot overflow"
)]
mod tests {
    use super::*;
    use crate::audio::AudioDevice;
    use crate::playback::{PlaybackOutputs, Playout};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// What the test's sound devices played, by device.
    #[derive(Default)]
    struct Chart {
        writes: Mutex<BTreeMap<String, Vec<i16>>>,
        opened: Mutex<Vec<String>>,
        closed: Mutex<Vec<String>>,
        refuse: Mutex<Option<String>>,
        default: Mutex<String>,
        fail_on_write: AtomicBool,
    }

    impl Chart {
        fn writes(&self, device: &str) -> Vec<i16> {
            self.writes
                .lock()
                .unwrap()
                .get(device)
                .cloned()
                .unwrap_or_default()
        }
        fn opened(&self) -> Vec<String> {
            self.opened.lock().unwrap().clone()
        }
        fn closed(&self) -> Vec<String> {
            self.closed.lock().unwrap().clone()
        }
    }

    struct Recording(Arc<Chart>);

    impl SinkFactory for Recording {
        fn open(&self, device: &str) -> Result<OpenSink, String> {
            let device = if device == DEFAULT_SINK {
                self.0.default.lock().unwrap().clone()
            } else {
                device.to_owned()
            };
            if self.0.refuse.lock().unwrap().as_deref() == Some(device.as_str()) {
                return Err("the device is gone".to_owned());
            }
            self.0.opened.lock().unwrap().push(device.clone());
            Ok(OpenSink {
                stream: Box::new(Stream {
                    chart: Arc::clone(&self.0),
                    device: device.clone(),
                }),
                device,
            })
        }
    }

    struct Stream {
        chart: Arc<Chart>,
        device: String,
    }

    impl SinkStream for Stream {
        fn write(&mut self, samples: &[i16]) -> Result<(), String> {
            if self.chart.fail_on_write.load(Ordering::Acquire) && self.device == "Desk" {
                return Err("the speaker was unplugged".to_owned());
            }
            self.chart
                .writes
                .lock()
                .unwrap()
                .entry(self.device.clone())
                .or_default()
                .extend_from_slice(samples);
            Ok(())
        }

        fn close(&mut self) {
            self.chart.closed.lock().unwrap().push(self.device.clone());
        }
    }

    fn sink(id: &str, device: &str, volume: f32, sync_ms: i64) -> Output {
        Output {
            id: id.to_owned(),
            kind: OutputKind::Sink {
                device: device.to_owned(),
            },
            label: device.to_owned(),
            volume,
            sync_ms,
            state: OutputState::Playing,
        }
    }

    /// 10 ms of the same sample: long enough for the delay lines to show their offset.
    fn block(sample: i16) -> Vec<i16> {
        vec![sample; (audio_output::RATE as usize / 100) * CHANNELS]
    }

    #[test]
    fn two_devices_play_at_once_each_at_its_own_volume_and_distance() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("desk", "Desk", 0.5, 10));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("both devices open");

        let block_len = (audio_output::RATE as usize / 100) * CHANNELS;
        let behind = (audio_output::RATE as usize / 100) * CHANNELS;
        playout.host_mut().write(&block(1000));
        assert_eq!(chart.writes("Built-in"), block(1000));
        // The desk speaker stands 10 ms behind the built-in one, so its first block is the
        // silence it waits out and then the first half of what it was given, at its own volume.
        let mut waited = vec![0_i16; behind];
        waited.extend(vec![500_i16; block_len - behind]);
        assert_eq!(chart.writes("Desk"), waited);
        playout.host_mut().write(&block(1000));
        assert_eq!(
            chart.writes("Desk")[block_len..],
            vec![500_i16; block_len],
            "only its own volume remains, at its own distance"
        );
    }

    #[test]
    fn moving_an_output_keeps_every_speaker_at_its_own_sync_and_adding_starts_only_the_new_one() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("desk", "Desk", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("two devices");

        let mut next = outputs.clone();
        let desk = next.get("desk").unwrap().clone();
        next.move_to(desk, 0);
        next.set_sync("speaker", 20);
        next.add(sink("patio", "Patio", 1.0, 0));
        playout.apply(&next).expect("the patio speaker starts");
        assert_eq!(chart.opened(), ["Built-in", "Desk", "Patio"]);

        // The built-in speaker's sync holds it back while the ones that follow play at once.
        playout.host_mut().write(&block(2000));
        assert!(chart.writes("Desk").iter().all(|sample| *sample == 2000));
        let waiting = chart.writes("Built-in");
        assert!(
            waiting.iter().all(|sample| *sample == 0),
            "the built-in speaker waits behind the desk one by its sync"
        );
        assert_eq!(waiting.len(), block(2000).len());
    }

    #[test]
    fn the_same_output_naming_another_device_moves_its_stream() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("the built-in speaker opens");

        let mut next = outputs.clone();
        next.add(sink("speaker", "Desk", 1.0, 0));
        playout.apply(&next).expect("the desk speaker takes over");
        assert_eq!(chart.opened(), ["Built-in", "Desk"]);
        assert_eq!(chart.closed(), ["Built-in"]);
        playout.host_mut().write(&block(5));
        assert_eq!(chart.writes("Desk"), block(5));
    }

    #[test]
    fn a_jump_drops_the_sound_each_speaker_stood_behind() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("desk", "Desk", 1.0, 10));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("both devices open");
        playout.host_mut().write(&block(1000));
        playout.host_mut().flush();
        playout.host_mut().write(&block(2000));
        playout.host_mut().write(&block(2000));
        assert!(
            chart.writes("Desk").iter().all(|sample| *sample != 1000),
            "the desk speaker does not replay what it stood behind"
        );
        assert_eq!(
            chart.writes("Built-in"),
            [block(1000), block(2000), block(2000)].concat()
        );
    }

    #[test]
    fn a_device_that_will_not_open_is_said_and_the_rest_play() {
        let chart = Arc::new(Chart::default());
        *chart.refuse.lock().unwrap() = Some("Desk".to_owned());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("desk", "Desk", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        let failed = playout
            .apply(&outputs)
            .expect_err("the desk speaker is gone");
        assert_eq!(failed, ["desk: the device is gone"]);
        assert_eq!(playout.host().playing(), [("speaker", "Built-in")]);
        playout.host_mut().write(&block(700));
        assert_eq!(chart.writes("Built-in"), block(700));
    }

    #[test]
    fn a_speaker_that_stops_stops_alone_and_is_reported() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("desk", "Desk", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("two devices");

        chart.fail_on_write.store(true, Ordering::Release);
        playout.host_mut().write(&block(100));
        let events = playout.host_mut().events();
        assert_eq!(
            events,
            [HostEvent {
                id: "desk".to_owned(),
                state: OutputState::Failed,
                detail: Some("the speaker was unplugged".to_owned()),
            }]
        );
        assert_eq!(playout.host().playing(), [("speaker", "Built-in")]);
        chart.fail_on_write.store(false, Ordering::Release);
        playout.host_mut().write(&block(200));
        assert_eq!(
            chart.writes("Built-in"),
            [block(100), block(200)].concat(),
            "the other speaker keeps every later piece"
        );
    }

    #[test]
    fn an_output_that_follows_the_default_moves_when_the_default_does() {
        let chart = Arc::new(Chart::default());
        *chart.default.lock().unwrap() = "Built-in".to_owned();
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("sound", DEFAULT_SINK, 1.0, 0));
        outputs.add(sink("desk", "Desk", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("both devices");
        assert_eq!(
            playout.host().playing(),
            [("desk", "Desk"), ("sound", "Built-in")]
        );

        let graph = graph_of(&[("Built-in", true), ("Headphones", false), ("Desk", false)]);
        playout.host_mut().follow(&graph);
        assert!(
            playout.host_mut().events().is_empty(),
            "the default did not change"
        );

        let moved = graph_of(&[("Built-in", false), ("Headphones", true), ("Desk", false)]);
        *chart.default.lock().unwrap() = "Headphones".to_owned();
        playout.host_mut().follow(&moved);
        assert_eq!(
            playout.host_mut().events(),
            [HostEvent {
                id: "sound".to_owned(),
                state: OutputState::Playing,
                detail: None,
            }]
        );
        assert_eq!(
            playout.host().playing(),
            [("desk", "Desk"), ("sound", "Headphones")]
        );

        let unplugged = graph_of(&[("Built-in", false), ("Headphones", true)]);
        playout.host_mut().follow(&unplugged);
        assert_eq!(
            playout.host_mut().events(),
            [HostEvent {
                id: "desk".to_owned(),
                state: OutputState::Failed,
                detail: Some("That speaker went away.".to_owned()),
            }]
        );
    }

    #[test]
    fn ending_a_playback_closes_every_stream_it_opened() {
        let chart = Arc::new(Chart::default());
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0, 0));
        outputs.add(sink("patio", "Patio", 1.0, 0));
        let mut playout = Playout::new(SinkHost::new(Recording(Arc::clone(&chart))));
        playout.apply(&outputs).expect("two devices");
        drop(playout);
        let mut closed = chart.closed();
        closed.sort();
        assert_eq!(closed, ["Built-in", "Patio"]);
    }

    #[test]
    fn a_speaker_group_plays_through_its_set_never_as_a_device_of_its_own() {
        let graph = graph_of(&[("Built-in", true), (audio::SPEAKER_GROUP_SINK, false)]);
        let devices = devices_of(&graph);
        assert_eq!(
            devices,
            [SinkDevice {
                name: "Built-in".to_owned(),
                label: "Built-in".to_owned(),
                serial: 0,
                is_default: true,
            }]
        );
    }

    fn graph_of(devices: &[(&str, bool)]) -> AudioGraph {
        AudioGraph {
            sinks: devices
                .iter()
                .enumerate()
                .map(|(index, (name, is_default))| AudioDevice {
                    id: u32::try_from(index).unwrap(),
                    serial: u32::try_from(index).unwrap(),
                    name: (*name).to_owned(),
                    description: (*name).to_owned(),
                    is_default: *is_default,
                    volume: None,
                    muted: None,
                    channels: 2,
                    codec: None,
                })
                .collect(),
            ..AudioGraph::default()
        }
    }
}
