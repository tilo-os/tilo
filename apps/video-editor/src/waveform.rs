//! Waveform peaks, read from the source's own samples: an uncompressed WAV
//! directly, everything else decoded to PCM under `--features decode`.
//!
//! When no samples can be had — an un-hydrated reference, no audio, no decoder
//! linked — the answer is [`WaveformState::Unavailable`] and the lane is drawn
//! as a flat line. Never a synthesized envelope: it cannot be told apart from
//! real peaks, so it would hide the absence it stands for.
//!
//! [`WaveformCache`] computes each source's peaks once, off the UI thread;
//! [`WaveformCache::warm_blocking`] keeps captures deterministic.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::media::MediaSource;
use crate::model::MediaId;
use ui::lock::MutexRecover as _;

/// Peak resolution per source; the renderer samples this to fit a clip.
pub const WAVE_BUCKETS: usize = 512;

/// Peaks cover only the read prefix, so a very long source cannot stall the
/// compute; for typical clips this is the whole file.
const READ_CAP: u64 = 48 * 1024 * 1024;

/// An envelope needs no more, which keeps a long track's PCM small.
#[cfg(feature = "decode")]
const PEAK_RATE: u32 = 8_000;

/// A pathological file must not pin the worker.
#[cfg(feature = "decode")]
const DECODE_BUDGET: Duration = Duration::from_secs(30);

/// `buckets` normalized peak amplitudes (0..1). `None` when no samples could be
/// had, which the caller must show as absence.
pub fn peaks_from_file(path: &Path, buckets: usize) -> Option<Vec<f32>> {
    let data = crate::files::read_prefix(path, READ_CAP).ok()?;
    if let Some(p) = peaks_from_wav_bytes(&data, buckets) {
        return Some(p);
    }
    peaks_by_decoding(path, buckets)
}

#[cfg(not(feature = "decode"))]
fn peaks_by_decoding(_path: &Path, _buckets: usize) -> Option<Vec<f32>> {
    None
}

/// Reads through [`media::Source`], so a file under the mount hydrates on
/// demand, as playback does.
#[cfg(feature = "decode")]
fn peaks_by_decoding(path: &Path, buckets: usize) -> Option<Vec<f32>> {
    // A peak per short window while decoding, then down to `buckets`: the max at
    // each stage keeps the true peak and memory flat.
    const WINDOW: usize = 256;

    let mut windows: Vec<f32> = Vec::new();
    let mut acc = 0.0f32;
    let mut in_window = 0usize;
    media::pcm::decode(
        &crate::media::engine_source(path),
        PEAK_RATE,
        1,
        DECODE_BUDGET,
        |samples| {
            for v in samples {
                acc = acc.max(v.abs());
                in_window += 1;
                if in_window == WINDOW {
                    windows.push(acc);
                    acc = 0.0;
                    in_window = 0;
                }
            }
        },
    );
    // A budget that ran out or a damaged stream still leaves the peaks of
    // everything decoded before it.
    if in_window > 0 {
        windows.push(acc);
    }
    (!windows.is_empty()).then(|| bucketize(&windows, buckets))
}

pub fn peaks_from_wav_bytes(d: &[u8], buckets: usize) -> Option<Vec<f32>> {
    let buckets = buckets.max(1);
    let layout = crate::probe::wav_layout(d)?;
    let (fmt, channels, bits) = (layout.format, layout.channels, layout.bits);
    let (start, size) = layout.data?;
    let end = start.saturating_add(size).min(d.len());
    if end <= start {
        return None;
    }
    let samples = decode_samples(d.get(start..end)?, fmt, bits, channels)?;
    if samples.is_empty() {
        return None;
    }
    Some(bucketize(&samples, buckets))
}

/// Interleaved PCM as mono `f32` in -1..1.
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
        // Each sample is exactly `bits / 8` bytes, so every arm matches its
        // `(fmt, bits)`.
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

/// `buckets` peak amplitudes (max abs per bucket), scaled so the loudest is 1.0.
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

/// What the renderer may draw for one source. There is no "draw something
/// plausible" case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformState {
    /// Nothing has been asked for yet.
    Unrequested,
    /// Draw the same flat line as [`WaveformState::Unavailable`].
    Pending,
    /// Fetch the peaks with [`WaveformCache::peaks`].
    Ready,
    /// No samples: an un-hydrated reference, no audio, or no decoder linked.
    Unavailable,
}

struct Task {
    id: MediaId,
    path: PathBuf,
}

/// One worker, one decode at a time: a timeline full of audio clips must not
/// stand up a decode per clip at once.
#[derive(Default)]
struct Work {
    queue: VecDeque<Task>,
    running: bool,
    shutdown: bool,
}

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

    /// Called each sync.
    pub fn drain(&mut self) {
        while let Ok((id, res)) = self.rx.try_recv() {
            let slot = match res {
                Some(p) => Slot::Ready(Arc::new(p)),
                // Terminal: the samples cannot be had, so it is never asked
                // again.
                None => Slot::Missing,
            };
            self.map.insert(id, slot);
        }
    }

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

    /// Synchronous, for deterministic captures and tests.
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

    pub fn peaks(&self, id: MediaId) -> Option<&Arc<Vec<f32>>> {
        match self.map.get(&id) {
            Some(Slot::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// What the renderer may draw. A `None` from [`Self::peaks`] is no licence
    /// to synthesize something.
    pub fn state(&self, id: MediaId) -> WaveformState {
        match self.map.get(&id) {
            None => WaveformState::Unrequested,
            Some(Slot::Pending) => WaveformState::Pending,
            Some(Slot::Ready(_)) => WaveformState::Ready,
            Some(Slot::Missing) => WaveformState::Unavailable,
        }
    }

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

/// Drains the queue one source at a time and retires when idle.
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
        {
            // A download cut short can end anywhere: inside a chunk header or
            // mid-sample. None of those may read past the end.
            let wav = wav_16(&[0.5, -0.5, 0.25, -0.25], 8000);
            for n in 0..wav.len() {
                let _ = peaks_from_wav_bytes(&wav[..n], 4);
            }
            assert_eq!(peaks_from_wav_bytes(&wav[..44], 4), None);
            assert!(peaks_from_wav_bytes(&wav, 4).is_some());
        }
        {
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
        {
            assert!(peaks_from_wav_bytes(b"not a wav file at all!!", 32).is_none());
            assert!(peaks_from_wav_bytes(&[], 32).is_none());
        }
        {
            // Two signals must give two envelopes.
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
        {
            // The real queue and worker: a readable WAV must land as Ready and
            // `has_pending` must fall back to false.
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

            tilo_test_env::wait_until("the worker's terminal answer", || {
                cache.drain();
                !cache.has_pending()
            });
            assert_eq!(cache.state(id), WaveformState::Ready);
            let peaks = cache.peaks(id).expect("real peaks");
            assert_eq!(peaks.len(), WAVE_BUCKETS);
            assert!(peaks.iter().copied().fold(0.0, f32::max) > 0.9);
            std::fs::remove_file(&f).ok();
        }
    }

    #[test]
    fn a_source_with_no_readable_audio_settles_as_unavailable() {
        // A real file that is not WAV and holds no audio: absence, not a shape.
        let f = std::env::temp_dir().join(format!("tilo-ve-noaudio-{}.bin", std::process::id()));
        std::fs::write(&f, vec![0x5Au8; 4096]).unwrap();
        let mut cache = WaveformCache::new();
        let id = uuid::Uuid::new_v4();
        let source = crate::media::MediaSource {
            abs_path: f.clone(),
            hydration: crate::media::Hydration::Local,
        };
        assert_eq!(cache.state(id), WaveformState::Unrequested);
        cache.warm_blocking(id, &source);
        assert_eq!(cache.state(id), WaveformState::Unavailable);
        assert!(cache.peaks(id).is_none());
        assert!(!cache.has_pending(), "a settled failure is not pending");
        std::fs::remove_file(&f).ok();
    }
}
