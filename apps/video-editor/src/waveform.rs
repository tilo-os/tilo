//! Real audio waveform peaks, read from the source's own samples.
//!
//! A waveform must be the signal or nothing. Two producers, in order:
//!
//!   * **Uncompressed WAV** (PCM 8/16/24/32-bit and 32-bit float) is read
//!     directly with no decoder, so peaks are real even on a build with no
//!     media stack.
//!   * **Everything else** (MP3, AAC, Opus, FLAC, the audio track of a video
//!     …) is decoded to PCM under `--features decode`, through a `GStreamer`
//!     `uridecodebin → appsink` chain fed by the shared [`media::Source`]
//!     seam, off the UI thread and bounded to one decode at a time.
//!
//! When neither can produce samples — an un-hydrated reference, a file with
//! no audio, no decoder linked — the answer is [`WaveformState::Unavailable`]
//! and the renderer must draw a flat neutral line. It must **not** substitute
//! a synthesized envelope: a sum of sines looks exactly like a waveform and
//! carries none of the source's information, which is worse than an obviously
//! empty lane because it cannot be told apart from the real thing.
//!
//! [`WaveformCache`] keeps the result per source and computes it off the UI
//! thread for the live app (delivered over a channel, drained each frame); a
//! synchronous [`WaveformCache::warm_blocking`] path keeps `--shots` captures
//! deterministic.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::media::MediaSource;
use crate::model::MediaId;
use ui::lock::MutexRecover as _;

/// Peak resolution stored per source; the renderer samples this to fit a clip.
pub const WAVE_BUCKETS: usize = 512;

/// Cap on bytes read for peak computation so a very long source can't stall the
/// compute. Peaks cover the read prefix; for typical clips this is the whole file.
const READ_CAP: u64 = 48 * 1024 * 1024;

/// Sample rate the decode path resamples to before measuring peaks. An
/// envelope needs no more; it keeps a long track's PCM small and the decode
/// cheap.
#[cfg(feature = "decode")]
const PEAK_RATE: u32 = 8_000;

/// Wall-clock ceiling on one decode, so a pathological file cannot pin the
/// worker.
#[cfg(feature = "decode")]
const DECODE_BUDGET: Duration = Duration::from_secs(30);

/// Compute `buckets` normalized peak amplitudes (0..1) from the real audio at
/// `path`: PCM read directly for a WAV, a real decode for anything else.
///
/// `None` means no samples could be obtained — the caller must show that as
/// absence, never as a stand-in shape.
pub fn peaks_from_file(path: &Path, buckets: usize) -> Option<Vec<f32>> {
    let data = crate::files::read_prefix(path, READ_CAP).ok()?;
    if let Some(p) = peaks_from_wav_bytes(&data, buckets) {
        return Some(p);
    }
    peaks_by_decoding(path, buckets)
}

/// Decode `path`'s first audio stream to PCM and measure its real peaks.
/// Compiled out without the decode feature, where it is honestly `None`.
#[cfg(not(feature = "decode"))]
fn peaks_by_decoding(_path: &Path, _buckets: usize) -> Option<Vec<f32>> {
    None
}

/// Decode `path`'s first audio stream to mono PCM through
/// `uridecodebin → audioconvert → audioresample → appsink` and reduce it to
/// `buckets` real peaks.
///
/// The URI comes from the shared [`media::Source`] seam, so a file on the
/// `/var/workspace` FUSE is read *through* the mount and hydrates on demand,
/// exactly as playback does.
#[cfg(feature = "decode")]
fn peaks_by_decoding(path: &Path, buckets: usize) -> Option<Vec<f32>> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    // Two-stage reduction: a peak per short window while pulling, then down to
    // `buckets`. Taking the max at each stage preserves the true peak exactly
    // while keeping memory flat for a long track.
    const WINDOW: usize = 256;

    gst::init().ok()?;
    let uri = crate::media::engine_source(path).to_uri().ok()?;

    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .ok()?;
    let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("channels", 1i32)
        .field("rate", PEAK_RATE as i32)
        .build();
    let appsink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
        .max_buffers(64)
        .drop(false)
        .build();

    let pipeline = gst::Pipeline::new();
    let sink_el: gst::Element = appsink.clone().upcast();
    pipeline
        .add_many([&src, &convert, &resample, &sink_el])
        .ok()?;
    gst::Element::link_many([&convert, &resample, &sink_el]).ok()?;

    // `uridecodebin` only grows its source pads once it knows the stream, so
    // the audio branch is linked when the pad appears — and only the first
    // audio stream is taken.
    let convert_weak = convert.downgrade();
    src.connect_pad_added(move |_, pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = convert.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        let is_audio = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|st| st.name().starts_with("audio/")))
            .unwrap_or(false);
        if is_audio {
            let _ = pad.link(&sink_pad);
        }
    });

    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return None;
    }

    // A source the pipeline cannot read (no audio, unknown container) reports
    // an error on the bus and never reaches EOS at the sink; stop there rather
    // than polling the sink until the budget runs out.
    let Some(bus) = pipeline.bus() else {
        let _ = pipeline.set_state(gst::State::Null);
        return None;
    };
    let windows = pull_peak_windows(&appsink, &bus, WINDOW);
    let _ = pipeline.set_state(gst::State::Null);
    Some(bucketize(&windows?, buckets))
}

/// Pull decoded mono S16 samples from `appsink` and reduce each `window` of
/// them to its peak, until EOS, a pipeline error on `bus`, or the decode
/// budget. `None` when no samples arrived at all.
#[cfg(feature = "decode")]
fn pull_peak_windows(
    appsink: &gstreamer_app::AppSink,
    bus: &gstreamer::Bus,
    window: usize,
) -> Option<Vec<f32>> {
    use gstreamer as gst;

    let mut windows: Vec<f32> = Vec::new();
    let mut acc = 0.0f32;
    let mut in_window = 0usize;
    let deadline = ui::time::now() + DECODE_BUDGET;
    let mut saw_any = false;
    loop {
        if ui::time::now() > deadline {
            break;
        }
        if bus.pop_filtered(&[gst::MessageType::Error]).is_some() {
            break;
        }
        match appsink.try_pull_sample(gst::ClockTime::from_mseconds(200)) {
            Some(sample) => {
                let Some(buffer) = sample.buffer() else {
                    continue;
                };
                let Ok(map) = buffer.map_readable() else {
                    continue;
                };
                for &pair in map.as_slice().as_chunks::<2>().0 {
                    let v = f32::from(i16::from_le_bytes(pair)) / 32768.0;
                    let a = v.abs();
                    if a > acc {
                        acc = a;
                    }
                    in_window += 1;
                    if in_window == window {
                        windows.push(acc);
                        acc = 0.0;
                        in_window = 0;
                    }
                }
                saw_any = true;
            }
            None => {
                if appsink.is_eos() {
                    break;
                }
            }
        }
    }
    if in_window > 0 {
        windows.push(acc);
    }
    (saw_any && !windows.is_empty()).then_some(windows)
}

/// Pure peak computation from WAV bytes (unit-testable on crafted PCM).
pub fn peaks_from_wav_bytes(d: &[u8], buckets: usize) -> Option<Vec<f32>> {
    let buckets = buckets.max(1);
    if !d.starts_with(b"RIFF") || d.get(8..12) != Some(b"WAVE".as_slice()) {
        return None;
    }
    let mut i = 12usize;
    let (mut fmt, mut channels, mut bits) = (0u16, 1u16, 16u16);
    let mut data_range: Option<(usize, usize)> = None;
    while let Some(&[i0, i1, i2, i3, s0, s1, s2, s3]) = d.get(i..i + 8) {
        let size = u32::from_le_bytes([s0, s1, s2, s3]) as usize;
        let body = i + 8;
        match &[i0, i1, i2, i3] {
            b"fmt " => {
                if let Some(&[f0, f1, c0, c1, .., b0, b1]) = d.get(body..body + 16) {
                    fmt = u16::from_le_bytes([f0, f1]);
                    channels = u16::from_le_bytes([c0, c1]).max(1);
                    bits = u16::from_le_bytes([b0, b1]);
                }
            }
            b"data" => {
                let end = (body + size).min(d.len());
                if end > body {
                    data_range = Some((body, end));
                }
            }
            _ => {}
        }
        i = body + size + (size & 1);
    }
    let (start, end) = data_range?;
    // `body < end <= d.len()` when the range was recorded.
    let samples = decode_samples(d.get(start..end)?, fmt, bits, channels)?;
    if samples.is_empty() {
        return None;
    }
    Some(bucketize(&samples, buckets))
}

/// Decode interleaved PCM into a mono (channel-averaged) `f32` stream in -1..1.
fn decode_samples(d: &[u8], fmt: u16, bits: u16, channels: u16) -> Option<Vec<f32>> {
    let ch = channels as usize;
    let bytes_per = (bits / 8) as usize;
    let frame = bytes_per * ch;
    if frame == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(d.len() / frame);
    for frame_bytes in d.chunks_exact(frame) {
        let mut acc = 0.0f32;
        // Every sample is exactly `bits / 8` bytes, so each arm's slice
        // pattern matches whenever its `(fmt, bits)` does.
        for sample in frame_bytes.chunks_exact(bytes_per) {
            let v = match (fmt, bits, sample) {
                (1, 8, &[b]) => (f32::from(b) - 128.0) / 128.0,
                (1, 16, &[b0, b1]) => f32::from(i16::from_le_bytes([b0, b1])) / 32768.0,
                (1, 24, &[b0, b1, b2]) => {
                    let mut x = i32::from(b0) | (i32::from(b1) << 8) | (i32::from(b2) << 16);
                    if x & 0x0080_0000 != 0 {
                        x |= !0x00FF_FFFF;
                    }
                    x as f32 / 8_388_608.0
                }
                (1, 32, &[b0, b1, b2, b3]) => {
                    i32::from_le_bytes([b0, b1, b2, b3]) as f32 / 2_147_483_648.0
                }
                (3, 32, &[b0, b1, b2, b3]) => f32::from_le_bytes([b0, b1, b2, b3]),
                _ => return None,
            };
            acc += v;
        }
        out.push(acc / ch as f32);
    }
    Some(out)
}

/// Reduce a sample stream to `buckets` peak amplitudes (max abs per bucket),
/// normalized so the loudest bucket is 1.0.
fn bucketize(samples: &[f32], buckets: usize) -> Vec<f32> {
    let mut peaks = vec![0.0f32; buckets];
    let n = samples.len();
    for (idx, &s) in samples.iter().enumerate() {
        let b = (idx * buckets / n).min(buckets - 1);
        let a = s.abs();
        // `b <= buckets - 1`, the last slot.
        if let Some(peak) = peaks.get_mut(b)
            && a > *peak
        {
            *peak = a;
        }
    }
    let maxp = peaks.iter().copied().fold(0.0f32, f32::max);
    if maxp > 1e-6 {
        for p in &mut peaks {
            *p /= maxp;
        }
    }
    peaks
}

enum Slot {
    Pending,
    Ready(Arc<Vec<f32>>),
    Missing,
}

/// What the renderer may draw for one source's audio. Exhaustive on purpose:
/// there is no "draw something plausible" case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformState {
    /// Nothing has been asked for yet.
    Unrequested,
    /// A real compute is running. Draw the same neutral line as
    /// [`WaveformState::Unavailable`] — the answer simply is not in yet.
    Pending,
    /// Real peaks are cached; fetch them with [`WaveformCache::peaks`].
    Ready,
    /// The samples cannot be obtained: an un-hydrated reference, a source with
    /// no audio, or no decoder linked. Draw a flat neutral placeholder and
    /// **never** a synthesized envelope.
    Unavailable,
}

/// One queued peak computation.
struct Task {
    id: MediaId,
    path: PathBuf,
}

/// The single background worker's shared state. One worker, one decode at a
/// time: peak extraction runs a real pipeline, and a timeline full of audio
/// clips must not stand up one per clip at once.
#[derive(Default)]
struct Work {
    queue: VecDeque<Task>,
    running: bool,
    shutdown: bool,
}

/// Per-source waveform cache: computes peaks once, off the UI thread, and holds
/// the result for the renderer.
pub struct WaveformCache {
    map: HashMap<MediaId, Slot>,
    work: Arc<(Mutex<Work>, Condvar)>,
    tx: Sender<(MediaId, Option<Vec<f32>>)>,
    rx: Receiver<(MediaId, Option<Vec<f32>>)>,
}

impl Default for WaveformCache {
    fn default() -> Self {
        Self::new()
    }
}

/// How long the idle worker waits before retiring.
const WORKER_IDLE: Duration = Duration::from_secs(20);

impl WaveformCache {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            map: HashMap::new(),
            work: Arc::new((Mutex::new(Work::default()), Condvar::new())),
            tx,
            rx,
        }
    }

    /// Pull completed background computations into the cache. Call each `sync`.
    pub fn drain(&mut self) {
        while let Ok((id, res)) = self.rx.try_recv() {
            let slot = match res {
                Some(p) => Slot::Ready(Arc::new(p)),
                // Terminal: the samples are not obtainable. Recorded so the
                // request stops being pending and is never re-issued.
                None => Slot::Missing,
            };
            self.map.insert(id, slot);
        }
    }

    /// Queue a background compute for `id` if it hasn't been requested yet.
    pub fn request(&mut self, id: MediaId, source: &MediaSource) {
        if self.map.contains_key(&id) {
            return;
        }
        if !source.is_decodable() {
            self.map.insert(id, Slot::Missing);
            return;
        }
        self.map.insert(id, Slot::Pending);
        let task = Task {
            id,
            path: source.abs_path.clone(),
        };
        let (lock, cv) = &*self.work;
        let mut w = lock.lock_recover();
        w.queue.push_back(task);
        if !w.running {
            w.running = true;
            let work = self.work.clone();
            let tx = self.tx.clone();
            // Without a thread (the page) waveforms are simply not drawn.
            let _ = ui::rt::spawn_thread("video-editor-waveform", move || run_worker(&work, &tx));
        }
        drop(w);
        cv.notify_one();
    }

    /// Compute `id`'s peaks synchronously (deterministic `--shots`/tests).
    pub fn warm_blocking(&mut self, id: MediaId, source: &MediaSource) {
        if matches!(self.map.get(&id), Some(Slot::Ready(_) | Slot::Missing)) {
            return;
        }
        let peaks = if source.is_decodable() {
            peaks_from_file(&source.abs_path, WAVE_BUCKETS)
        } else {
            None
        };
        let slot = match peaks {
            Some(p) => Slot::Ready(Arc::new(p)),
            None => Slot::Missing,
        };
        self.map.insert(id, slot);
    }

    /// The ready peaks for `id`, or `None` while pending / unavailable.
    pub fn peaks(&self, id: MediaId) -> Option<&Arc<Vec<f32>>> {
        match self.map.get(&id) {
            Some(Slot::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// What the renderer may draw for `id`. Use this rather than treating a
    /// `None` from [`Self::peaks`] as licence to synthesize something.
    pub fn state(&self, id: MediaId) -> WaveformState {
        match self.map.get(&id) {
            None => WaveformState::Unrequested,
            Some(Slot::Pending) => WaveformState::Pending,
            Some(Slot::Ready(_)) => WaveformState::Ready,
            Some(Slot::Missing) => WaveformState::Unavailable,
        }
    }

    /// Whether any requested waveform is still computing (so the app keeps
    /// syncing until it lands).
    pub fn has_pending(&self) -> bool {
        self.map.values().any(|s| matches!(s, Slot::Pending))
    }
}

impl Drop for WaveformCache {
    fn drop(&mut self) {
        let (lock, cv) = &*self.work;
        let mut w = lock.lock_recover();
        w.shutdown = true;
        w.queue.clear();
        drop(w);
        cv.notify_all();
    }
}

/// The single peak worker: drains the queue one source at a time and retires
/// when idle.
fn run_worker(work: &(Mutex<Work>, Condvar), tx: &Sender<(MediaId, Option<Vec<f32>>)>) {
    let (lock, cv) = work;
    loop {
        let task = {
            let mut w = lock.lock_recover();
            if w.shutdown {
                w.running = false;
                return;
            }
            let task = if let Some(task) = w.queue.pop_front() {
                task
            } else {
                let (next_work, res) = cv
                    .wait_timeout(w, WORKER_IDLE)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                w = next_work;
                if w.shutdown {
                    w.running = false;
                    return;
                }
                if let Some(task) = w.queue.pop_front() {
                    task
                } else {
                    if res.timed_out() {
                        w.running = false;
                        return;
                    }
                    continue; // spurious wake
                }
            };
            drop(w);
            task
        };
        let peaks = peaks_from_file(&task.path, WAVE_BUCKETS);
        if tx.send((task.id, peaks)).is_err() {
            let mut w = lock.lock_recover();
            w.running = false;
            drop(w);
            return; // the cache is gone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real 16-bit PCM mono WAV with the given samples (-1..1).
    fn wav_16(samples: &[f32], sr: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let byte_rate = sr * 2;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&sr.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    #[test]
    fn wav_peaks_decode_every_format_and_prefix_safely() {
        // Case `every_prefix_of_a_wav_is_read_without_panicking`.
        {
            // A download cut short can end anywhere: inside the chunk header,
            // inside `fmt `, or mid-sample. None of those may read past the end.
            let wav = wav_16(&[0.5, -0.5, 0.25, -0.25], 8000);
            for n in 0..wav.len() {
                let _ = peaks_from_wav_bytes(&wav[..n], 4);
            }
            // With the samples cut off entirely there is nothing to draw.
            assert_eq!(peaks_from_wav_bytes(&wav[..44], 4), None);
            assert!(peaks_from_wav_bytes(&wav, 4).is_some());
        }
        // Case `every_supported_sample_format_decodes_to_the_same_level`.
        {
            // Half scale, as each PCM width spells it.
            let cases: [(u16, u16, Vec<u8>); 5] = [
                (1, 8, vec![192]),
                (1, 16, 16_384i16.to_le_bytes().to_vec()),
                (1, 24, vec![0x00, 0x00, 0x40]),
                (1, 32, 1_073_741_824i32.to_le_bytes().to_vec()),
                (3, 32, 0.5f32.to_le_bytes().to_vec()),
            ];
            for (fmt, bits, bytes) in cases {
                let got = decode_samples(&bytes, fmt, bits, 1).expect("a supported format");
                assert_eq!(got.len(), 1, "{fmt}/{bits}");
                assert!((got[0] - 0.5).abs() < 1e-6, "{fmt}/{bits}: {}", got[0]);
            }
            // Negative 24-bit samples sign-extend rather than reading as huge.
            let neg = decode_samples(&[0x00, 0x00, 0xC0], 1, 24, 1).expect("24-bit");
            assert!((neg[0] + 0.5).abs() < 1e-6, "{}", neg[0]);
            // An unknown width is refused, not guessed at.
            assert_eq!(decode_samples(&[0, 0], 1, 12, 1), None);
            // A partial trailing frame is dropped, not read past.
            let stereo = decode_samples(&[0, 0x40, 0, 0x40, 0], 1, 16, 2).expect("stereo");
            assert_eq!(stereo.len(), 1);
        }
        // Case `peaks_reflect_the_real_pcm_envelope`.
        {
            // Loud first half, quiet second half → the peaks must mirror that shape.
            let sr = 8000;
            let n = sr as usize; // 1s
            let mut samples = vec![0.0f32; n];
            for (i, s) in samples.iter_mut().enumerate() {
                let amp = if i < n / 2 { 0.9 } else { 0.1 };
                *s = amp * (i as f32 * 0.2).sin();
            }
            let wav = wav_16(&samples, sr);
            let peaks = peaks_from_wav_bytes(&wav, 16).expect("real WAV → peaks");
            assert_eq!(peaks.len(), 16);
            // Normalized: some bucket hits ~1.0 (the loud half).
            assert!(peaks.iter().copied().fold(0.0, f32::max) > 0.98);
            // The first-half buckets are clearly louder than the second-half ones.
            let first: f32 = peaks[..8].iter().sum();
            let second: f32 = peaks[8..].iter().sum();
            assert!(
                first > second * 3.0,
                "loud half dominates ({first} vs {second})"
            );
        }
        // Case `non_wav_bytes_yield_no_peaks`.
        {
            assert!(peaks_from_wav_bytes(b"not a wav file at all!!", 32).is_none());
            assert!(peaks_from_wav_bytes(&[], 32).is_none());
        }
        // Case `peaks_track_the_source_and_are_not_a_fixed_shape`.
        {
            // Two different signals must give two different envelopes. A
            // synthesized fallback would give the same picture for both — that is
            // exactly what makes it indistinguishable from real data and wrong.
            let sr = 8000;
            let rising: Vec<f32> = (0..sr)
                .map(|i| (i as f32 / sr as f32) * (i as f32 * 0.2).sin())
                .collect();
            let falling: Vec<f32> = rising.iter().rev().copied().collect();
            let a = peaks_from_wav_bytes(&wav_16(&rising, sr as u32), 32).unwrap();
            let b = peaks_from_wav_bytes(&wav_16(&falling, sr as u32), 32).unwrap();
            assert_ne!(a, b, "different audio must produce different peaks");
            assert!(a[28] > a[3], "a rising signal rises");
            assert!(b[3] > b[28], "a falling signal falls");
        }
    }

    #[test]
    fn waveform_cache_settles_real_missing_and_unavailable_sources() {
        // Case `cache_warm_blocking_reads_a_real_file`.
        {
            let dir = std::env::temp_dir();
            let f = dir.join(format!("tilo-ve-wave-{}.wav", std::process::id()));
            let samples: Vec<f32> = (0..4000).map(|i| 0.7 * (i as f32 * 0.1).sin()).collect();
            std::fs::write(&f, wav_16(&samples, 8000)).unwrap();

            let mut cache = WaveformCache::new();
            let id = uuid::Uuid::new_v4();
            let source = crate::media::MediaSource {
                abs_path: f.clone(),
                hydration: crate::media::Hydration::Local,
            };
            assert!(cache.peaks(id).is_none(), "nothing cached yet");
            cache.warm_blocking(id, &source);
            let peaks = cache.peaks(id).expect("warmed peaks");
            assert_eq!(peaks.len(), WAVE_BUCKETS);
            assert!(
                peaks.iter().copied().fold(0.0, f32::max) > 0.9,
                "a real signal is present"
            );
            std::fs::remove_file(&f).ok();
        }
        // Case `unhydrated_source_is_missing_not_pending`.
        {
            let mut cache = WaveformCache::new();
            let id = uuid::Uuid::new_v4();
            let source = crate::media::MediaSource {
                abs_path: "/definitely/not/here.wav".into(),
                hydration: crate::media::Hydration::Projected,
            };
            cache.request(id, &source);
            assert!(
                !cache.has_pending(),
                "an un-hydrated source never spawns a compute"
            );
            assert!(cache.peaks(id).is_none());
            assert_eq!(
                cache.state(id),
                WaveformState::Unavailable,
                "the renderer is told there is nothing, so it draws nothing"
            );
        }
        // Case `a_queued_compute_reaches_a_terminal_state_and_goes_quiet`.
        {
            // Drive the real queue + worker, not a stub: a readable WAV must land
            // as Ready and `has_pending` must fall back to false.
            let f = std::env::temp_dir().join(format!("tilo-ve-queue-{}.wav", std::process::id()));
            let samples: Vec<f32> = (0..6000).map(|i| 0.8 * (i as f32 * 0.05).sin()).collect();
            std::fs::write(&f, wav_16(&samples, 8000)).unwrap();
            let mut cache = WaveformCache::new();
            let id = uuid::Uuid::new_v4();
            let source = crate::media::MediaSource {
                abs_path: f.clone(),
                hydration: crate::media::Hydration::Local,
            };
            cache.request(id, &source);
            assert!(cache.has_pending(), "the request is in flight");
            assert_eq!(cache.state(id), WaveformState::Pending);

            let deadline = tilo_test_env::hang_deadline();
            while cache.has_pending() && ui::time::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
                cache.drain();
            }
            assert!(
                !cache.has_pending(),
                "the worker delivered a terminal answer"
            );
            assert_eq!(cache.state(id), WaveformState::Ready);
            let peaks = cache.peaks(id).expect("real peaks");
            assert_eq!(peaks.len(), WAVE_BUCKETS);
            assert!(peaks.iter().copied().fold(0.0, f32::max) > 0.9);
            std::fs::remove_file(&f).ok();
        }
    }

    #[test]
    fn a_source_with_no_readable_audio_settles_as_unavailable() {
        // A real file that is not WAV and holds no decodable audio. With no
        // decoder linked this is immediate; with one, the pipeline finds no
        // audio stream. Either way the answer is absence, never a shape.
        let f = std::env::temp_dir().join(format!("tilo-ve-noaudio-{}.bin", std::process::id()));
        std::fs::write(&f, vec![0x5Au8; 4096]).unwrap();
        let mut cache = WaveformCache::new();
        let id = uuid::Uuid::new_v4();
        let source = crate::media::MediaSource {
            abs_path: f.clone(),
            hydration: crate::media::Hydration::Local,
        };
        assert_eq!(cache.state(id), WaveformState::Unrequested);
        #[cfg(feature = "decode")]
        let started = ui::time::now();
        cache.warm_blocking(id, &source);
        assert_eq!(cache.state(id), WaveformState::Unavailable);
        // The failed pipeline settles on its own error, not by holding the
        // worker until the decode budget runs out.
        #[cfg(feature = "decode")]
        assert!(
            started.elapsed() < DECODE_BUDGET / 2,
            "an unreadable source settled only after {:?}",
            started.elapsed()
        );
        assert!(cache.peaks(id).is_none());
        assert!(!cache.has_pending(), "a settled failure is not pending");
        std::fs::remove_file(&f).ok();
    }
}
