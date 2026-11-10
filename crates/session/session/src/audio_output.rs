//! An app's sound on this device's output.
//!
//! A hosted app receives neither an audio socket nor the device's mixer: it hands this session the
//! PCM it decodes (20 ms of stereo 48 kHz at a time, [`runtime_collab::audio_output`]) and this
//! session renders it on the device's output, one stream per open output, named after the app so a
//! volume mixer shows it there. The stream opens only for a caller the broker re-attests before
//! every command, and the person's lockdown silences it where it silences every other sound
//! ([`media::audio_output::is_disabled`]): at its next write.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use media::audio_output::{CHANNELS, MAX_SAMPLES, RATE};
use media::pw_stream::{AudioStreams, PlaybackConfig, PlaybackSource, PlaybackStream};
use media::sync_playout::ChannelRole;
use runtime_collab::audio_output::{Command, Snapshot};
use tilo_action_broker::services::{
    AudioOutputFuture, AudioOutputServices, NativeMediaRequestContext,
};

/// How many apps may sound at once on one desktop.
const MAX_OUTPUTS: usize = 8;
/// The most PCM one output holds while the device catches up: 7680 frames, as the wire names it.
const MAX_QUEUED_SAMPLES: usize = MAX_SAMPLES * 8;
/// The name the device's mixer shows an app's output under.
const NODE_NAME: &str = "tilo-app-output";

/// What one open output belongs to: the connected process, and the route its record carries.
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

/// What the device is about to play, as the app handed it over.
struct Pcm {
    queue: VecDeque<i16>,
    paused: bool,
    gain: f32,
    /// Frames the device has taken since this output opened.
    rendered: u64,
    /// The last packet this output accepted.
    sequence: u64,
    last: Option<blake3::Hash>,
}

impl Pcm {
    fn new() -> Self {
        Self {
            queue: VecDeque::with_capacity(MAX_QUEUED_SAMPLES),
            paused: false,
            gain: 1.0,
            rendered: 0,
            sequence: 0,
            last: None,
        }
    }
}

/// What the device pulls from: the queue, as the app filled it.
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
        let gain = pcm.gain;
        let mut rendered = 0_u64;
        for sample in out {
            let Some(value) = pcm.queue.pop_front() else {
                break;
            };
            *sample = f32::from(value) / 32768.0 * gain;
            rendered = rendered.saturating_add(1);
        }
        pcm.rendered = pcm.rendered.saturating_add(rendered / u64::from(CHANNELS));
    }
}

/// One output an app opened: what it handed over, and the stream playing it.
struct Entry {
    generation: u64,
    /// The admission this output was opened under, asked before every command.
    revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pcm: Arc<Mutex<Pcm>>,
    alive: Arc<AtomicBool>,
    stream: Option<Box<dyn PlaybackStream>>,
}

impl Entry {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let pcm = self
            .pcm
            .lock()
            .map_err(|_| "audio output state unavailable".to_owned())?;
        if let Some(failure) = self.stream.as_ref().and_then(|stream| stream.failure()) {
            return Err(failure);
        }
        let alive = self.alive.load(Ordering::Acquire);
        Ok(Snapshot {
            generation: self.generation,
            accepted_sequence: pcm.sequence,
            rendered_frames: pcm.rendered,
            queued_frames: u32::try_from(pcm.queue.len() / usize::from(CHANNELS))
                .unwrap_or(u32::MAX),
            paused: pcm.paused,
            ready: alive
                && self
                    .stream
                    .as_ref()
                    .is_some_and(|stream| stream.timing().is_some()),
            closed: !alive,
        })
    }

    /// Stop playing, and let a thread of its own join the stream: a `PipeWire` teardown never runs
    /// on the lane that answers the app.
    fn fence(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut pcm) = self.pcm.lock() {
            pcm.queue.clear();
        }
        if let Some(stream) = self.stream.take()
            && let Err(error) = std::thread::Builder::new()
                .name("tilo-app-output-retire".into())
                .spawn(move || drop(stream))
        {
            // The stream was joined on this lane instead, which is what the thread was there to
            // keep off it: rare, and never silent.
            tracing::warn!(%error, "an app's output stream could not be retired off this lane");
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.fence();
    }
}

/// The outputs open on this device, by the connection and route that opened them.
#[derive(Default)]
struct Outputs {
    next: u64,
    entries: BTreeMap<Key, Entry>,
}

/// What the door holds of this session's audio output.
struct Shared {
    outputs: Mutex<Outputs>,
    streams: &'static dyn AudioStreams,
}

impl Shared {
    /// Open an output for the connection `key` names, playing on a stream of its own, as the
    /// admission `revalidate` asks.
    async fn open(
        &self,
        key: Key,
        revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
        app: String,
    ) -> Result<Snapshot, String> {
        revalidate()?;
        if media::audio_output::is_disabled() {
            return Err("sound output is disabled by the person's lockdown policy".into());
        }
        let pcm = Arc::new(Mutex::new(Pcm::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let generation = {
            let mut outputs = self
                .outputs
                .lock()
                .map_err(|_| "audio output state unavailable".to_owned())?;
            if !outputs.entries.contains_key(&key) && outputs.entries.len() >= MAX_OUTPUTS {
                return Err("audio output capacity reached".into());
            }
            let generation = outputs
                .next
                .checked_add(1)
                .ok_or("audio output generation exhausted")?;
            outputs.next = generation;
            outputs.entries.insert(
                key.clone(),
                Entry {
                    generation,
                    revalidate,
                    pcm: Arc::clone(&pcm),
                    alive: Arc::clone(&alive),
                    stream: None,
                },
            );
            generation
        };
        let config = PlaybackConfig::for_role(
            NODE_NAME,
            &format!("{app}, an app on Tilo"),
            RATE,
            ChannelRole::Stereo,
        );
        let opened = self.play(config, pcm, alive).await;
        let playing = self.playing(&key, generation, opened);
        if playing.is_err() {
            // An open that ended before it played leaves nothing of its own behind.
            self.retire(&key, generation);
        }
        playing
    }

    /// The device's stream for `config`, opened off the lane that answers the app: it waits on
    /// `PipeWire`.
    async fn play(
        &self,
        config: PlaybackConfig,
        pcm: Arc<Mutex<Pcm>>,
        alive: Arc<AtomicBool>,
    ) -> Result<Box<dyn PlaybackStream>, String> {
        let streams = self.streams;
        tokio::task::spawn_blocking(move || {
            streams
                .open_playback(&config, Box::new(Source { pcm, alive }))
                .map_err(|error| format!("the device's output did not open: {error}"))
        })
        .await
        .map_err(|_| "the device's output open ended".to_owned())?
    }

    /// Hand `opened` to the output `key` opened as `generation`, unless it was superseded or
    /// stopped meanwhile.
    fn playing(
        &self,
        key: &Key,
        generation: u64,
        opened: Result<Box<dyn PlaybackStream>, String>,
    ) -> Result<Snapshot, String> {
        let mut outputs = self
            .outputs
            .lock()
            .map_err(|_| "audio output state unavailable".to_owned())?;
        let entry = outputs
            .entries
            .get_mut(key)
            .filter(|entry| entry.generation == generation)
            .ok_or("audio output was superseded")?;
        if !entry.alive.load(Ordering::Acquire) {
            return Err("the output stopped before it played".into());
        }
        (entry.revalidate)()?;
        entry.stream = Some(opened?);
        entry.snapshot()
    }

    /// Take the output `key` opened as `generation` out of the registry.
    fn retire(&self, key: &Key, generation: u64) {
        if let Ok(mut outputs) = self.outputs.lock()
            && outputs
                .entries
                .get(key)
                .is_some_and(|entry| entry.generation == generation)
        {
            outputs.entries.remove(key);
        }
    }

    /// What `command` does to the output `context`'s connection opened.
    fn control(
        &self,
        key: &Key,
        revalidate: &(dyn Fn() -> Result<(), String> + Send + Sync),
        command: Command,
    ) -> Result<Snapshot, String> {
        if let Err(error) = revalidate() {
            // The connection this output belongs to is no longer admitted: nothing it opened
            // sounds any more.
            self.stop(Some(key.peer));
            return Err(error);
        }
        let generation = match &command {
            Command::Open {} => return Err("an output opens with `open`".into()),
            Command::Write { generation, .. }
            | Command::Pause { generation, .. }
            | Command::Volume { generation, .. }
            | Command::Flush { generation }
            | Command::Status { generation }
            | Command::Close { generation } => *generation,
        };
        // The admission this output was opened under can have ended since; it is asked outside the
        // registry's lock, because it waits on membership and custody.
        let retained = self.caller(key, generation)?;
        if let Err(error) = retained() {
            self.stop(Some(key.peer));
            return Err(error);
        }
        let mut outputs = self
            .outputs
            .lock()
            .map_err(|_| "audio output state unavailable".to_owned())?;
        let entry = outputs
            .entries
            .get_mut(key)
            .filter(|entry| entry.generation == generation)
            .ok_or("stale audio output generation")?;
        if media::audio_output::is_disabled() {
            entry.fence();
            return Err("sound output is disabled by the person's lockdown policy".into());
        }
        if matches!(command, Command::Close { .. }) {
            entry.fence();
            let snapshot = entry.snapshot()?;
            outputs.entries.remove(key);
            return Ok(snapshot);
        }
        match command {
            Command::Write {
                sequence, samples, ..
            } => write(entry, sequence, samples)?,
            Command::Pause { paused, .. } => {
                entry
                    .pcm
                    .lock()
                    .map_err(|_| "audio output state unavailable".to_owned())?
                    .paused = paused;
            }
            Command::Volume { percent, .. } => {
                entry
                    .pcm
                    .lock()
                    .map_err(|_| "audio output state unavailable".to_owned())?
                    .gain = f32::from(percent) / 100.0;
            }
            Command::Flush { .. } => entry
                .pcm
                .lock()
                .map_err(|_| "audio output state unavailable".to_owned())?
                .queue
                .clear(),
            Command::Status { .. } => {}
            Command::Open {} | Command::Close { .. } => {
                return Err("an output opens and closes as it is told".into());
            }
        }
        entry.snapshot()
    }

    /// The admission of the output `key` opened as `generation`.
    fn caller(
        &self,
        key: &Key,
        generation: u64,
    ) -> Result<Arc<dyn Fn() -> Result<(), String> + Send + Sync>, String> {
        let outputs = self
            .outputs
            .lock()
            .map_err(|_| "audio output state unavailable".to_owned())?;
        outputs
            .entries
            .get(key)
            .filter(|entry| entry.generation == generation)
            .map(|entry| Arc::clone(&entry.revalidate))
            .ok_or_else(|| "stale audio output generation".to_owned())
    }

    /// Stop what `peer` opened, or every output when it is `None`.
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

/// Take one packet the app wrote: a repeat of the last one is taken once, a gap or an out-of-order
/// packet is refused, and a full queue drops the packet rather than holding the app's decoder.
fn write(entry: &mut Entry, sequence: u64, samples: Vec<i16>) -> Result<(), String> {
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let digest = blake3::hash(&bytes);
    let mut pcm = entry
        .pcm
        .lock()
        .map_err(|_| "audio output state unavailable".to_owned())?;
    if sequence == pcm.sequence && pcm.last == Some(digest) {
        return Ok(());
    }
    if Some(sequence) != pcm.sequence.checked_add(1) {
        return Err("stale or reordered audio output packet".into());
    }
    if pcm.queue.len().saturating_add(samples.len()) > MAX_QUEUED_SAMPLES {
        return Ok(());
    }
    pcm.queue.extend(samples);
    pcm.sequence = sequence;
    pcm.last = Some(digest);
    Ok(())
}

/// The apps' sound on this device's output, as the action door reaches it.
#[derive(Clone)]
pub(crate) struct AppAudioOutput(Arc<Shared>);

impl AppAudioOutput {
    /// The service over this device's own output.
    pub(crate) fn new() -> Self {
        Self::over(media::pw_stream::native_streams())
    }

    fn over(streams: &'static dyn AudioStreams) -> Self {
        Self(Arc::new(Shared {
            outputs: Mutex::new(Outputs::default()),
            streams,
        }))
    }
}

impl AudioOutputServices for AppAudioOutput {
    fn output(&self, context: NativeMediaRequestContext, command: Command) -> AudioOutputFuture {
        if !command.valid() {
            return Box::pin(async { Err("invalid audio output command".into()) });
        }
        let key = Key::from(&context);
        let app = context.app_id().to_owned();
        let revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync> =
            Arc::new(move || context.revalidate());
        match command {
            Command::Open {} => {
                let this = self.clone();
                Box::pin(async move { this.0.open(key, revalidate, app).await })
            }
            command => {
                let outcome = self.0.control(&key, &*revalidate, command);
                Box::pin(async move { outcome })
            }
        }
    }

    fn disconnect(&self, peer_id: u64) {
        self.0.stop(Some(peer_id));
    }
}

#[cfg(test)]
mod tests {
    //! What an app's sound comes to on this device, over a device the test plays from: the PCM it
    //! hands over reaches the speakers, its controls take effect, and an output whose admission
    //! ended stops.

    use super::*;

    use std::sync::mpsc;

    use media::pw_stream::{CaptureConfig, CaptureStream, DeviceTiming, StreamError};

    /// The device's output as the test drives it: what it was opened for, and what it pulls.
    struct Device {
        chart: Mutex<Vec<Opened>>,
        ended: Mutex<Vec<mpsc::Receiver<()>>>,
    }

    struct Opened {
        description: String,
        source: Arc<Mutex<Box<dyn PlaybackSource>>>,
        running: Arc<AtomicBool>,
    }

    impl Device {
        fn last(&self) -> std::sync::MutexGuard<'_, Vec<Opened>> {
            self.chart.lock().unwrap()
        }

        /// Wait for output `at`'s stream to end.
        fn ended(&self, at: usize) {
            let ended =
                self.ended.lock().unwrap()[at].recv_timeout(std::time::Duration::from_secs(5));
            assert!(ended.is_ok(), "the device's stream ends with its output");
        }
    }

    impl AudioStreams for Device {
        fn open_playback(
            &self,
            config: &PlaybackConfig,
            source: Box<dyn PlaybackSource>,
        ) -> Result<Box<dyn PlaybackStream>, StreamError> {
            let running = Arc::new(AtomicBool::new(true));
            let (stopped, ended) = mpsc::channel();
            self.ended.lock().unwrap().push(ended);
            self.chart.lock().unwrap().push(Opened {
                description: config.description.clone(),
                source: Arc::new(Mutex::new(source)),
                running: Arc::clone(&running),
            });
            Ok(Box::new(Output { running, stopped }))
        }

        fn open_capture(
            &self,
            _config: &CaptureConfig,
        ) -> Result<Box<dyn CaptureStream>, StreamError> {
            Err(StreamError::Unavailable("output only".into()))
        }
    }

    /// One output of the device, stopped when it is dropped.
    struct Output {
        running: Arc<AtomicBool>,
        stopped: mpsc::Sender<()>,
    }

    impl PlaybackStream for Output {
        fn timing(&self) -> Option<DeviceTiming> {
            self.running
                .load(Ordering::Acquire)
                .then_some(DeviceTiming {
                    now_ns: 0,
                    delay_ns: 0,
                    queued_frames: 0,
                })
        }

        fn failure(&self) -> Option<String> {
            None
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            let _ = self.stopped.send(());
        }
    }

    /// A device kept for the test's life, and the service playing on it.
    fn device() -> (&'static Device, AppAudioOutput) {
        let device: &'static Device = Box::leak(Box::new(Device {
            chart: Mutex::new(Vec::new()),
            ended: Mutex::new(Vec::new()),
        }));
        (device, AppAudioOutput::over(device))
    }

    fn key(peer: u64) -> Key {
        Key {
            peer,
            app: "so.tilo.music".into(),
            workspace: "w1".into(),
            record: "so.tilo.music/songs".into(),
        }
    }

    fn admitted() -> Arc<dyn Fn() -> Result<(), String> + Send + Sync> {
        Arc::new(|| Ok(()))
    }

    /// What the device plays of `output`, one 20 ms quantum at a time.
    fn rendered(device: &Device, at: usize, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0_f32; frames * usize::from(CHANNELS)];
        let chart = device.last();
        let mut source = chart[at].source.lock().unwrap();
        source.render(0, &mut out);
        out
    }

    #[tokio::test]
    async fn the_pcm_an_app_hands_over_plays_on_the_device() {
        let (device, audio) = device();
        let opened = audio
            .0
            .open(key(1), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        assert_eq!(opened.generation, 1);
        assert_eq!(device.last().len(), 1, "one stream for the app");
        assert!(
            device.last()[0].description.starts_with("so.tilo.music"),
            "the mixer shows the app it plays for"
        );
        let write = |sequence, samples: [i16; 4]| Command::Write {
            generation: 1,
            sequence,
            samples: samples.to_vec(),
        };
        audio
            .0
            .control(
                &key(1),
                &*admitted(),
                write(1, [16384, 16384, 16384, 16384]),
            )
            .unwrap();
        let played = rendered(device, 0, 2);
        assert_eq!(
            played,
            vec![0.5, 0.5, 0.5, 0.5],
            "the device plays what was handed over"
        );
        let status = audio
            .0
            .control(&key(1), &*admitted(), Command::Status { generation: 1 })
            .unwrap();
        assert_eq!(status.accepted_sequence, 1);
        assert_eq!(
            status.rendered_frames, 2,
            "the device counted what it pulled"
        );
        assert!(status.ready, "an open stream is ready");
        assert!(!status.closed);
    }

    #[tokio::test]
    async fn a_repeat_is_taken_once_and_a_gap_is_refused() {
        let (_device, audio) = device();
        audio
            .0
            .open(key(1), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        let packet = |sequence| Command::Write {
            generation: 1,
            sequence,
            samples: vec![1, 1],
        };
        audio.0.control(&key(1), &*admitted(), packet(1)).unwrap();
        audio
            .0
            .control(&key(1), &*admitted(), packet(1))
            .expect("a retried packet is taken once");
        let queued = |audio: &AppAudioOutput| {
            let outputs = audio.0.outputs.lock().unwrap();
            outputs
                .entries
                .get(&key(1))
                .unwrap()
                .snapshot()
                .unwrap()
                .queued_frames
        };
        assert_eq!(queued(&audio), 1, "the retry added nothing");
        assert!(
            audio.0.control(&key(1), &*admitted(), packet(3)).is_err(),
            "a packet that skipped one is refused rather than played out of order"
        );
    }

    #[tokio::test]
    async fn pause_flush_and_volume_shape_what_the_device_plays() {
        let (device, audio) = device();
        audio
            .0
            .open(key(1), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        let write = |sequence| Command::Write {
            generation: 1,
            sequence,
            samples: vec![8000, 8000],
        };
        audio.0.control(&key(1), &*admitted(), write(1)).unwrap();
        audio
            .0
            .control(
                &key(1),
                &*admitted(),
                Command::Pause {
                    generation: 1,
                    paused: true,
                },
            )
            .unwrap();
        assert_eq!(
            rendered(device, 0, 1),
            vec![0.0, 0.0],
            "a paused output fills silence"
        );
        audio
            .0
            .control(
                &key(1),
                &*admitted(),
                Command::Pause {
                    generation: 1,
                    paused: false,
                },
            )
            .unwrap();
        audio
            .0
            .control(
                &key(1),
                &*admitted(),
                Command::Volume {
                    generation: 1,
                    percent: 50,
                },
            )
            .unwrap();
        let played = rendered(device, 0, 1);
        assert!(
            (played[0] - 8000.0 / 32768.0 / 2.0).abs() < 1e-6,
            "half the gain it was given: {played:?}"
        );
        audio.0.control(&key(1), &*admitted(), write(2)).unwrap();
        audio
            .0
            .control(&key(1), &*admitted(), Command::Flush { generation: 1 })
            .unwrap();
        let flushed = audio
            .0
            .control(&key(1), &*admitted(), Command::Status { generation: 1 })
            .unwrap();
        assert_eq!(
            flushed.queued_frames, 0,
            "a flush drops what has not played"
        );
    }

    #[tokio::test]
    async fn an_output_stops_when_its_admission_ends_or_its_process_goes() {
        let (_device, audio) = device();
        audio
            .0
            .open(key(1), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        audio
            .0
            .open(key(2), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        let ended: Arc<dyn Fn() -> Result<(), String> + Send + Sync> =
            Arc::new(|| Err("committed read launcher custody ended".into()));
        let refused = audio.0.control(
            &key(1),
            &*ended,
            Command::Write {
                generation: 1,
                sequence: 1,
                samples: vec![1, 1],
            },
        );
        assert!(
            refused.is_err(),
            "an admission that ended refuses every command"
        );
        assert_eq!(
            audio.0.outputs.lock().unwrap().entries.len(),
            1,
            "its output is stopped and the other app's is left playing"
        );
        audio.disconnect(2);
        assert!(
            audio.0.outputs.lock().unwrap().entries.is_empty(),
            "the process that went stops sounding"
        );
    }

    #[tokio::test]
    async fn closing_an_output_stops_its_stream_and_a_stale_generation_is_refused() {
        let (device, audio) = device();
        audio
            .0
            .open(key(1), admitted(), "so.tilo.music".into())
            .await
            .unwrap();
        let closed = audio
            .0
            .control(&key(1), &*admitted(), Command::Close { generation: 1 })
            .unwrap();
        assert!(closed.closed);
        device.ended(0);
        assert!(
            audio
                .0
                .control(&key(1), &*admitted(), Command::Status { generation: 1 })
                .is_err(),
            "a closed generation is gone"
        );
    }
}
