//! Real audio waveform peaks, read straight from PCM.
//!
//! An audio clip's waveform is computed from the source's **real samples**, not
//! a synthesized shape. WAV (PCM 8/16/24/32-bit and 32-bit float) is read
//! directly with no decoder, so peaks are real on the default hermetic build. A
//! source we can't read cheaply (a compressed file without the decode feature,
//! or an un-hydrated reference) yields `None`, and the timeline falls back to a
//! neutral placeholder rather than a fabricated signal.
//!
//! [`WaveformCache`] keeps the result per source and computes it **off the UI
//! thread** for the live app (delivered over a channel, drained each frame); a
//! synchronous [`WaveformCache::warm_blocking`] path keeps `--shots` captures
//! deterministic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::media::MediaSource;
use crate::model::MediaId;

/// Peak resolution stored per source; the renderer samples this to fit a clip.
pub const WAVE_BUCKETS: usize = 512;

/// Cap on bytes read for peak computation so a very long source can't stall the
/// compute. Peaks cover the read prefix; for typical clips this is the whole file.
const READ_CAP: u64 = 48 * 1024 * 1024;

/// Compute `buckets` normalized peak amplitudes (0..1) from the audio at `path`.
/// Returns `None` when the source isn't a readable PCM WAV.
pub fn peaks_from_file(path: &Path, buckets: usize) -> Option<Vec<f32>> {
    let len = std::fs::metadata(path).ok()?.len();
    let data = if len <= READ_CAP {
        std::fs::read(path).ok()?
    } else {
        use std::io::Read;
        let mut f = std::fs::File::open(path).ok()?;
        let mut buf = vec![0u8; READ_CAP as usize];
        let n = f.read(&mut buf).ok()?;
        buf.truncate(n);
        buf
    };
    peaks_from_wav_bytes(&data, buckets)
}

fn le_u16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        *d.get(at..at.checked_add(2)?)?.first_chunk::<2>()?,
    ))
}
fn le_u32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        *d.get(at..at.checked_add(4)?)?.first_chunk::<4>()?,
    ))
}

/// Pure peak computation from WAV bytes (unit-testable on crafted PCM).
pub fn peaks_from_wav_bytes(d: &[u8], buckets: usize) -> Option<Vec<f32>> {
    let buckets = buckets.max(1);
    if d.get(0..4) != Some(b"RIFF".as_slice()) || d.get(8..12) != Some(b"WAVE".as_slice()) {
        return None;
    }
    let mut i = 12usize;
    let (mut fmt, mut channels, mut bits) = (0u16, 1u16, 16u16);
    let mut data_range: Option<(usize, usize)> = None;
    while i + 8 <= d.len() {
        let id = d.get(i..i + 4)?;
        let size = le_u32(d, i + 4)? as usize;
        let body = i + 8;
        match id {
            b"fmt " if body + 16 <= d.len() => {
                fmt = le_u16(d, body)?;
                channels = le_u16(d, body + 2)?.max(1);
                bits = le_u16(d, body + 14)?;
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
    let frames = d.len() / frame;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * frame;
        let mut acc = 0.0f32;
        for c in 0..ch {
            let o = base + c * bytes_per;
            let v = match (fmt, bits) {
                (1, 8) => (f32::from(*d.get(o)?) - 128.0) / 128.0,
                (1, 16) => {
                    f32::from(i16::from_le_bytes(*d.get(o..o + 2)?.first_chunk::<2>()?)) / 32768.0
                }
                (1, 24) => {
                    let &[b0, b1, b2] = d.get(o..o + 3)?.first_chunk::<3>()?;
                    let mut x = i32::from(b0) | (i32::from(b1) << 8) | (i32::from(b2) << 16);
                    if x & 0x0080_0000 != 0 {
                        x |= !0x00FF_FFFF;
                    }
                    x as f32 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes(*d.get(o..o + 4)?.first_chunk::<4>()?) as f32
                        / 2_147_483_648.0
                }
                (3, 32) => f32::from_le_bytes(*d.get(o..o + 4)?.first_chunk::<4>()?),
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
        if let Some(slot) = peaks.get_mut(b)
            && a > *slot
        {
            *slot = a;
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

/// Per-source waveform cache: computes peaks once, off the UI thread, and holds
/// the result for the renderer.
pub struct WaveformCache {
    map: HashMap<MediaId, Slot>,
    tx: Sender<(MediaId, Option<Vec<f32>>)>,
    rx: Receiver<(MediaId, Option<Vec<f32>>)>,
}

impl Default for WaveformCache {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveformCache {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            map: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Pull completed background computations into the cache. Call each `sync`.
    pub fn drain(&mut self) {
        while let Ok((id, res)) = self.rx.try_recv() {
            let slot = match res {
                Some(p) => Slot::Ready(Arc::new(p)),
                None => Slot::Missing,
            };
            self.map.insert(id, slot);
        }
    }

    /// Kick off a background compute for `id` if it hasn't been requested yet.
    pub fn request(&mut self, id: MediaId, source: &MediaSource) {
        if self.map.contains_key(&id) {
            return;
        }
        if !source.is_decodable() {
            self.map.insert(id, Slot::Missing);
            return;
        }
        self.map.insert(id, Slot::Pending);
        let tx = self.tx.clone();
        let path = source.abs_path.clone();
        std::thread::spawn(move || {
            let peaks = peaks_from_file(&path, WAVE_BUCKETS);
            let _ = tx.send((id, peaks));
        });
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

    /// Whether any requested waveform is still computing (so the app keeps
    /// syncing until it lands).
    pub fn has_pending(&self) -> bool {
        self.map.values().any(|s| matches!(s, Slot::Pending))
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
    fn peaks_reflect_the_real_pcm_envelope() {
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

    #[test]
    fn non_wav_bytes_yield_no_peaks() {
        assert!(peaks_from_wav_bytes(b"not a wav file at all!!", 32).is_none());
        assert!(peaks_from_wav_bytes(&[], 32).is_none());
    }

    #[test]
    fn cache_warm_blocking_reads_a_real_file() {
        let dir = std::env::temp_dir();
        let f = dir.join(format!("qos-ve-wave-{}.wav", std::process::id()));
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

    #[test]
    fn unhydrated_source_is_missing_not_pending() {
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
    }
}
