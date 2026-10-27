//! Page audio: the engine's PCM, played through the governed output service.
//!
//! A sandboxed app is given no `PipeWire`/`Pulse` socket and no `/dev/snd`; it
//! plays sound by handing PCM to the host's output service
//! (`media::audio_output`, one output per application). The engine host runs
//! Chromium's audio in the embedder-handler mode, so each tab's decoded stream
//! arrives here as `audio_start` / `audio` / `audio_stop` events instead of
//! being opened on a device Chromium cannot reach:
//!
//! - `audio_start {rate, channels}` — a page began producing sound;
//! - `audio {pcm}` — base64 of interleaved little-endian `i16` frames;
//! - `audio_stop` — the page stopped (paused, muted by the page, navigated, or
//!   the tab closed).
//!
//! The service keeps **one** output per application and a second `open`
//! replaces the first, so tabs cannot each hold an output. All of a process's
//! tabs feed one [`Mixer`]: each tab's PCM is converted to the service's
//! 48 kHz stereo, buffered at most [`MAX_BUFFERED_FRAMES`] (the oldest PCM is
//! dropped so a stalled device never adds delay), and summed with the others
//! into the service's packets by a single worker. The output is opened with
//! the first sound and closed after [`IDLE_CLOSE`] without any.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use media::audio_output::{CHANNELS, MAX_SAMPLES, Output, RATE};
use serde_json::Value;
use tilo_lock::MutexRecover as _;

/// At most this much PCM waits per tab (100 ms): the latency ceiling.
const MAX_BUFFERED_FRAMES: usize = RATE as usize / 10;
/// How long the output stays open once every tab is silent.
const IDLE_CLOSE: Duration = Duration::from_secs(2);
/// Wait before retrying an output the service refused or lost.
const REOPEN_BACKOFF: Duration = Duration::from_secs(1);
/// One engine packet: far above Chromium's 10 ms, bounded for a hostile host.
const MAX_PACKET_FRAMES: usize = RATE as usize;
const MAX_INPUT_RATE: u32 = 384_000;
const MAX_INPUT_CHANNELS: u16 = 8;
const CHANNELS_USIZE: usize = CHANNELS as usize;

type Open = Arc<dyn Fn() -> Result<Box<dyn Output>, String> + Send + Sync>;

/// Converts one tab's PCM to 48 kHz stereo, keeping resampler state between
/// packets so a stream's seams do not click.
struct Converter {
    rate: u32,
    channels: usize,
    /// Position of the next output frame after `previous`, in 1/48000ths of
    /// an input frame.
    phase: u64,
    previous: (f32, f32),
}

impl Converter {
    fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels: usize::from(channels),
            phase: 0,
            previous: (0.0, 0.0),
        }
    }

    /// Append `input` (interleaved, `self.channels` wide) to `out` as
    /// interleaved stereo at 48 kHz.
    fn convert(&mut self, input: &[i16], out: &mut VecDeque<i16>) {
        let frames = input.chunks_exact(self.channels).map(|frame| {
            let left = f32::from(frame[0]);
            let right = if self.channels > 1 {
                f32::from(frame[1])
            } else {
                left
            };
            (left, right)
        });
        if self.rate == RATE {
            for (left, right) in frames {
                out.push_back(to_sample(left));
                out.push_back(to_sample(right));
            }
            return;
        }
        // The read position in input frames, counted in 1/RATE steps so it
        // advances by exactly `rate` per output frame and never drifts.
        let unit = u64::from(RATE);
        let mut line = Vec::with_capacity(input.len() / self.channels + 1);
        line.push(self.previous);
        line.extend(frames);
        let last = line.len() - 1;
        let end = last as u64 * unit;
        while self.phase < end {
            let index = (self.phase / unit) as usize;
            let fraction = (self.phase % unit) as f32 / RATE as f32;
            let (a, b) = (line[index], line[index + 1]);
            out.push_back(to_sample(a.0 + (b.0 - a.0) * fraction));
            out.push_back(to_sample(a.1 + (b.1 - a.1) * fraction));
            self.phase += u64::from(self.rate);
        }
        self.phase -= end;
        self.previous = line[last];
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is rounded and clamped into i16 range first"
)]
fn to_sample(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

struct Stream {
    converter: Converter,
    /// Interleaved 48 kHz stereo, oldest first.
    pcm: VecDeque<i16>,
    /// The tab stopped: leave once the buffered PCM has played.
    ended: bool,
}

#[derive(Default)]
struct State {
    streams: HashMap<u64, Stream>,
    next: u64,
    shutdown: bool,
}

impl State {
    /// The next packet: every stream's oldest PCM, summed (saturating), at
    /// most one service packet long. `None` while all are empty. Streams
    /// that ended and drained are dropped.
    fn take_mix(&mut self) -> Option<Vec<i16>> {
        let frames = self
            .streams
            .values()
            .map(|stream| stream.pcm.len() / CHANNELS_USIZE)
            .max()?
            .min(MAX_SAMPLES / CHANNELS_USIZE);
        let packet = (frames > 0).then(|| {
            let mut sum = vec![0_i32; frames * CHANNELS_USIZE];
            for stream in self.streams.values_mut() {
                for slot in &mut sum {
                    match stream.pcm.pop_front() {
                        Some(sample) => *slot += i32::from(sample),
                        None => break,
                    }
                }
            }
            sum.into_iter()
                .map(|sample| {
                    i16::try_from(sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                        .unwrap_or_default()
                })
                .collect()
        });
        self.streams
            .retain(|_, stream| !(stream.ended && stream.pcm.is_empty()));
        packet
    }
}

/// Every tab of this process, one output.
pub(super) struct Mixer {
    state: Mutex<State>,
    wake: Condvar,
    open: Open,
    idle_close: Duration,
}

impl Mixer {
    fn new(open: Open, idle_close: Duration) -> Arc<Self> {
        let mixer = Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            open,
            idle_close,
        });
        let worker = mixer.clone();
        let spawned = tilo_rt::spawn_thread("chromium-audio", move || worker.run());
        if let Err(error) = spawned {
            tracing::warn!(%error, "page audio worker did not start; pages stay silent");
        }
        mixer
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    fn attach(&self, rate: u32, channels: u16) -> u64 {
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        state.streams.insert(
            id,
            Stream {
                converter: Converter::new(rate, channels),
                pcm: VecDeque::new(),
                ended: false,
            },
        );
        id
    }

    fn push(&self, id: u64, samples: &[i16]) {
        let mut state = self.lock();
        if let Some(stream) = state.streams.get_mut(&id)
            && !stream.ended
        {
            stream.converter.convert(samples, &mut stream.pcm);
            let excess = stream
                .pcm
                .len()
                .saturating_sub(MAX_BUFFERED_FRAMES * CHANNELS_USIZE);
            stream.pcm.drain(..excess);
        }
        drop(state);
        self.wake.notify_one();
    }

    fn detach(&self, id: u64) {
        if let Some(stream) = self.lock().streams.get_mut(&id) {
            stream.ended = true;
        }
        self.wake.notify_one();
    }

    #[cfg(test)]
    fn shutdown(&self) {
        self.lock().shutdown = true;
        self.wake.notify_all();
    }

    fn run(&self) {
        let mut output: Option<Box<dyn Output>> = None;
        let mut silent_since = crate::time::now();
        let mut retry_after = None;
        loop {
            let packet = {
                let mut state = self.lock();
                loop {
                    if state.shutdown {
                        drop(state);
                        close(&mut output);
                        return;
                    }
                    if let Some(packet) = state.take_mix() {
                        break Some(packet);
                    }
                    if output.is_some() && silent_since.elapsed() >= self.idle_close {
                        break None;
                    }
                    let wait = if output.is_some() {
                        self.idle_close.saturating_sub(silent_since.elapsed())
                    } else {
                        Duration::from_secs(60)
                    };
                    state = self
                        .wake
                        .wait_timeout(state, wait.max(Duration::from_millis(1)))
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            };
            let Some(packet) = packet else {
                close(&mut output);
                continue;
            };
            silent_since = crate::time::now();
            if retry_after.is_some_and(|at: crate::time::Instant| at > silent_since) {
                continue;
            }
            if output.is_none() {
                match (self.open)() {
                    Ok(opened) => output = Some(opened),
                    Err(error) => {
                        tracing::warn!(%error, "page audio output unavailable");
                        retry_after = Some(silent_since + REOPEN_BACKOFF);
                        continue;
                    }
                }
            }
            if let Some(sink) = output.as_mut()
                && let Err(error) = sink.write(&packet)
            {
                tracing::warn!(%error, "page audio output failed; reopening");
                close(&mut output);
                retry_after = Some(crate::time::now() + REOPEN_BACKOFF);
            }
        }
    }
}

fn close(output: &mut Option<Box<dyn Output>>) {
    if let Some(mut sink) = output.take() {
        sink.close();
    }
}

/// The process-wide mixer, playing through the governed output service.
fn global() -> &'static Arc<Mixer> {
    static MIXER: OnceLock<Arc<Mixer>> = OnceLock::new();
    MIXER.get_or_init(|| Mixer::new(Arc::new(media::audio_output::open), IDLE_CLOSE))
}

/// One tab's audio: what its host events do to the mixer.
#[derive(Default)]
pub(super) struct PageAudio {
    stream: Mutex<Option<(Arc<Mixer>, u64, u16)>>,
}

impl PageAudio {
    /// Apply an `audio_start`, `audio` or `audio_stop` event to the process
    /// mixer.
    pub(super) fn event(&self, name: &str, value: &Value) {
        self.event_on(global, name, value);
    }

    fn event_on(&self, mixer: impl FnOnce() -> &'static Arc<Mixer>, name: &str, value: &Value) {
        match name {
            "audio_start" => {
                let (Some(rate), Some(channels)) = (rate(value), channels(value)) else {
                    return;
                };
                self.stop();
                let mixer = mixer().clone();
                let id = mixer.attach(rate, channels);
                *self.slot() = Some((mixer, id, channels));
            }
            "audio" => {
                let guard = self.slot();
                if let Some((mixer, id, channels)) = guard.as_ref()
                    && let Some(samples) = pcm(value, usize::from(*channels))
                {
                    mixer.push(*id, &samples);
                }
            }
            _ => self.stop(),
        }
    }

    /// The tab's stream is over; what it buffered still plays out.
    pub(super) fn stop(&self) {
        if let Some((mixer, id, _)) = self.slot().take() {
            mixer.detach(id);
        }
    }

    fn slot(&self) -> MutexGuard<'_, Option<(Arc<Mixer>, u64, u16)>> {
        self.stream.lock_recover()
    }
}

impl Drop for PageAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

fn rate(value: &Value) -> Option<u32> {
    let rate = u32::try_from(value["rate"].as_u64()?).ok()?;
    (1..=MAX_INPUT_RATE).contains(&rate).then_some(rate)
}

fn channels(value: &Value) -> Option<u16> {
    let channels = u16::try_from(value["channels"].as_u64()?).ok()?;
    (1..=MAX_INPUT_CHANNELS)
        .contains(&channels)
        .then_some(channels)
}

/// The interleaved samples of an `audio` event, whole frames of `channels`.
fn pcm(value: &Value, channels: usize) -> Option<Vec<i16>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value["pcm"].as_str()?)
        .ok()?;
    let frame = channels * 2;
    if bytes.is_empty() || bytes.len() % frame != 0 || bytes.len() / frame > MAX_PACKET_FRAMES {
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

/// Whether `value` is a well-formed audio event, for the transport's
/// validation (a malformed one is dropped, never a reason to fail the host).
pub(super) fn valid_event(name: &str, value: &Value) -> bool {
    match name {
        "audio_start" => rate(value).is_some() && channels(value).is_some(),
        "audio" => value["pcm"].as_str().is_some_and(|pcm| {
            pcm.len() <= MAX_PACKET_FRAMES * usize::from(MAX_INPUT_CHANNELS) * 3
        }),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Sink {
        writes: Mutex<Vec<Vec<i16>>>,
        opens: AtomicUsize,
        closes: AtomicUsize,
    }

    struct Recording(Arc<Sink>);

    impl Output for Recording {
        fn write(&mut self, samples: &[i16]) -> Result<media::audio_output::Observation, String> {
            self.0.writes.lock().unwrap().push(samples.to_vec());
            Ok(media::audio_output::Observation::default())
        }
        fn observation(&mut self) -> Result<media::audio_output::Observation, String> {
            Ok(media::audio_output::Observation::default())
        }
        fn cached_observation(&self) -> media::audio_output::Observation {
            media::audio_output::Observation::default()
        }
        fn cached_error(&self) -> Option<String> {
            None
        }
        fn pause(&mut self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn volume(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self) {
            self.0.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn mixer(idle: Duration) -> (Arc<Mixer>, Arc<Sink>) {
        let sink = Arc::new(Sink::default());
        let opened = sink.clone();
        let mixer = Mixer::new(
            Arc::new(move || {
                opened.opens.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Recording(opened.clone())) as Box<dyn Output>)
            }),
            idle,
        );
        (mixer, sink)
    }

    fn written(sink: &Sink) -> Vec<i16> {
        sink.writes.lock().unwrap().concat()
    }

    fn encode(samples: &[i16]) -> String {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn engine_pcm_reaches_the_governed_output_as_48k_stereo() {
        {
            let (mixer, sink) = mixer(IDLE_CLOSE);
            let id = mixer.attach(48_000, 1);
            mixer.push(id, &[1, 2, 3]);
            tilo_test_env::wait_until("the packet is written", || written(&sink).len() == 6);
            assert_eq!(written(&sink), [1, 1, 2, 2, 3, 3]);
            mixer.shutdown();
        }
        // Phase `a_24k_stream_is_resampled_to_twice_the_frames`.
        {
            let (mixer, sink) = mixer(IDLE_CLOSE);
            let id = mixer.attach(24_000, 2);
            let tone: Vec<i16> = (0..2400).flat_map(|n| [n as i16, -(n as i16)]).collect();
            mixer.push(id, &tone);
            tilo_test_env::wait_until("the resampled PCM is written", || {
                written(&sink).len() >= 2 * 4790
            });
            let samples = written(&sink);
            let frames = samples.len() / 2;
            assert!((4790..=4802).contains(&frames), "{frames} frames");
            // Linear interpolation: the ramp stays monotonic and in tune.
            assert!(samples.chunks(2).all(|f| f[0] == -f[1]));
            assert!(samples.chunks(2).map(|f| f[0]).is_sorted());
            mixer.shutdown();
        }
        {
            let (mixer, sink) = mixer(IDLE_CLOSE);
            let id = mixer.attach(48_000, 2);
            mixer.push(id, &vec![7; 4800 * 2]);
            tilo_test_env::wait_until("all PCM is written", || written(&sink).len() == 9600);
            assert!(
                sink.writes
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|packet| packet.len() <= MAX_SAMPLES && packet.len() % 2 == 0)
            );
            mixer.shutdown();
        }
    }

    #[test]
    fn streams_are_summed_and_a_stall_never_adds_delay() {
        {
            let mut state = State::default();
            for (id, level) in [(1, 20_000_i16), (2, 20_000), (3, -100)] {
                let mut stream = Stream {
                    converter: Converter::new(48_000, 2),
                    pcm: VecDeque::new(),
                    ended: false,
                };
                stream.pcm.extend([level; 4]);
                state.streams.insert(id, stream);
            }
            state.streams.get_mut(&3).unwrap().ended = true;
            assert_eq!(
                state.take_mix().unwrap(),
                [i16::MAX, i16::MAX, i16::MAX, i16::MAX]
            );
            assert!(
                !state.streams.contains_key(&3),
                "an ended, drained tab leaves"
            );
            assert!(state.take_mix().is_none());
        }
        {
            let gate = Arc::new(Mutex::new(()));
            let held = gate.lock().unwrap();
            let waiting = gate.clone();
            let mixer = Mixer::new(
                Arc::new(move || {
                    drop(waiting.lock().unwrap());
                    Err("device gone".into())
                }),
                IDLE_CLOSE,
            );
            let id = mixer.attach(48_000, 2);
            // The worker takes one packet and stalls opening the device; the
            // flood behind it is bounded, newest kept.
            for round in 0..50_i16 {
                mixer.push(id, &vec![round; 4800 * 2]);
            }
            let state = mixer.lock();
            let pcm = &state.streams[&id].pcm;
            assert!(
                pcm.len() <= MAX_BUFFERED_FRAMES * 2,
                "{} samples",
                pcm.len()
            );
            assert_eq!(pcm.back(), Some(&49), "the newest PCM is what stays");
            drop(state);
            drop(held);
            mixer.shutdown();
        }
    }

    #[test]
    fn the_output_opens_with_the_first_sound_and_closes_when_all_is_quiet() {
        let (mixer, sink) = mixer(Duration::from_millis(30));
        assert_eq!(
            sink.opens.load(Ordering::SeqCst),
            0,
            "nothing plays, nothing opens"
        );
        let id = mixer.attach(48_000, 2);
        mixer.push(id, &[5, 5]);
        mixer.detach(id);
        tilo_test_env::wait_until("the output closes", || {
            sink.closes.load(Ordering::SeqCst) == 1
        });
        assert_eq!(sink.opens.load(Ordering::SeqCst), 1);
        let id = mixer.attach(48_000, 2);
        mixer.push(id, &[6, 6]);
        tilo_test_env::wait_until("the output reopens", || {
            sink.opens.load(Ordering::SeqCst) == 2
        });
        mixer.shutdown();
    }

    #[test]
    fn host_events_drive_a_tabs_stream() {
        let (mixer, sink) = mixer(IDLE_CLOSE);
        let leaked: &'static Arc<Mixer> = Box::leak(Box::new(mixer.clone()));
        let tab = PageAudio::default();
        let send = |name: &str, value: Value| tab.event_on(|| leaked, name, &value);
        send("audio", json!({"pcm": encode(&[9, 9])}));
        send("audio_start", json!({"rate": 48000, "channels": 2}));
        send("audio", json!({"pcm": encode(&[1, -1, 2, -2])}));
        tilo_test_env::wait_until("the packet plays", || written(&sink) == [1, -1, 2, -2]);
        send("audio", json!({"pcm": "!!!"}));
        send("audio", json!({"pcm": encode(&[1, 2, 3])}));
        send("audio", json!({"pcm": ""}));
        send("audio_stop", json!({}));
        send("audio", json!({"pcm": encode(&[4, 4])}));
        // The next stream's packet plays after whatever the stopped one let
        // through.
        send("audio_start", json!({"rate": 48000, "channels": 2}));
        send("audio", json!({"pcm": encode(&[7, 7])}));
        tilo_test_env::wait_until("the next stream's packet plays", || {
            written(&sink).len() >= 6
        });
        assert_eq!(written(&sink), [1, -1, 2, -2, 7, 7]);
        assert!(!valid_event(
            "audio_start",
            &json!({"rate": 0, "channels": 2})
        ));
        assert!(!valid_event(
            "audio_start",
            &json!({"rate": 48000, "channels": 9})
        ));
        assert!(valid_event(
            "audio_start",
            &json!({"rate": 44100, "channels": 2})
        ));
        mixer.shutdown();
    }
}
