//! What a VM created with `--sound` played: QEMU's `wav` audio backend
//! appends the guest's output to `<disk>.wav` while a stream plays, so "the
//! sound kept playing" is measurable without anyone listening and without
//! this computer's own audio.

use std::path::Path;

use crate::error::CliError;

/// Where QEMU writes the sound output of the VM whose disk is `disk`.
#[must_use]
pub fn wav_path(disk: &str) -> String {
    format!("{disk}.wav")
}

/// Below this peak a stretch counts as silence: -60 dBFS, well under any
/// audible signal and above dither.
const SILENCE_DBFS: f64 = -60.0;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Measurement {
    pub sample_rate: u32,
    pub channels: u16,
    /// Seconds of sound the file holds.
    pub seconds: f64,
    /// The newest stretch measured, at most the window asked for.
    pub window_seconds: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub silent: bool,
}

/// Measure a WAV file as QEMU writes it: a RIFF header whose sizes stay zero
/// until QEMU closes the file, so the data reaches to the end of the file.
///
/// # Errors
/// The file is not 16- or 32-bit PCM WAV.
pub fn measure(bytes: &[u8], window: f64) -> Result<Measurement, String> {
    let (channels, sample_rate, bits, data) = pcm(bytes)?;
    let width = usize::from(bits / 8);
    let frame = width.saturating_mul(usize::from(channels));
    let frames = data.len().checked_div(frame).unwrap_or(0);
    let rate = f64::from(sample_rate);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative window in frames, capped at the recording's length"
    )]
    let window_frames = ((window.max(0.0) * rate) as usize).min(frames);
    let start = frames.saturating_sub(window_frames).saturating_mul(frame);
    let newest = data
        .get(start..frames.saturating_mul(frame))
        .unwrap_or_default();
    let full = if bits == 16 {
        f64::from(i16::MAX)
    } else {
        f64::from(i32::MAX)
    };
    let (mut peak, mut sum, mut count) = (0f64, 0f64, 0f64);
    for sample in newest.chunks_exact(width) {
        let value = match *sample {
            [a, b] => f64::from(i16::from_le_bytes([a, b])),
            [a, b, c, d] => f64::from(i32::from_le_bytes([a, b, c, d])),
            _ => 0.0,
        } / full;
        peak = peak.max(value.abs());
        sum += value * value;
        count += 1.0;
    }
    let db = |v: f64| {
        if v > 0.0 {
            20.0 * v.log10()
        } else {
            f64::NEG_INFINITY
        }
    };
    let rms = if count > 0.0 {
        (sum / count).sqrt()
    } else {
        0.0
    };
    let peak_dbfs = db(peak);
    #[expect(
        clippy::cast_precision_loss,
        reason = "frame counts of a recording fit an f64 exactly"
    )]
    let (seconds, window_seconds) = (frames as f64 / rate, window_frames as f64 / rate);
    Ok(Measurement {
        sample_rate,
        channels,
        seconds,
        window_seconds,
        peak_dbfs,
        rms_dbfs: db(rms),
        silent: peak_dbfs < SILENCE_DBFS,
    })
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The format and the sample bytes of a PCM WAV file.
fn pcm(bytes: &[u8]) -> Result<(u16, u32, u16, &[u8]), String> {
    if bytes.get(0..4) != Some(b"RIFF".as_slice()) || bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut at = 12usize;
    let mut format = None;
    loop {
        let (Some(id), Some(size)) = (
            bytes.get(at..at.saturating_add(4)),
            le32(bytes, at.saturating_add(4)),
        ) else {
            return Err("no data chunk".into());
        };
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        let body = at.saturating_add(8);
        match id {
            b"fmt " => {
                let field16 =
                    |o: usize| le16(bytes, body.saturating_add(o)).ok_or("short fmt chunk");
                let (tag, channels, bits) = (field16(0)?, field16(2)?, field16(14)?);
                let rate = le32(bytes, body.saturating_add(4)).ok_or("short fmt chunk")?;
                if tag != 1 || !matches!(bits, 16 | 32) || channels == 0 || rate == 0 {
                    return Err(format!(
                        "format {tag}, {bits}-bit, {channels} channels at {rate} Hz: only 16- or 32-bit PCM is measured"
                    ));
                }
                format = Some((channels, rate, bits));
            }
            b"data" => {
                let (channels, rate, bits) = format.ok_or("no fmt chunk before the data")?;
                // QEMU writes the sizes only when it closes the file.
                let end = if size == 0 {
                    bytes.len()
                } else {
                    body.saturating_add(size).min(bytes.len())
                };
                return Ok((
                    channels,
                    rate,
                    bits,
                    bytes.get(body..end).unwrap_or_default(),
                ));
            }
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
}

/// Read and [`measure`] `path`, with errors that say what to do.
///
/// # Errors
/// The file is missing or not a WAV file QEMU wrote.
pub fn measure_file(vm: &str, path: &Path, window: f64) -> Result<Measurement, CliError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::Unsupported {
                what: format!(
                    "measuring {vm}'s sound: {} does not exist, so nothing has played since the VM started",
                    path.display()
                ),
                alternative: "play something in the guest (pw-play or speaker-test there), then measure again; a VM created without --sound has no sound card: tilo-vm vm create <name> --sound".into(),
            });
        }
        Err(source) => {
            return Err(CliError::Io {
                context: format!("read {}", path.display()),
                source,
            });
        }
    };
    measure(&bytes, window).map_err(|detail| {
        CliError::InvalidInput(format!(
            "{} is not a recording QEMU wrote: {detail}",
            path.display()
        ))
    })
}

/// One line per measurement, for a person comparing two of them.
#[must_use]
pub fn describe(path: &Path, m: &Measurement) -> String {
    let level = if m.window_seconds == 0.0 {
        "nothing in it yet".to_owned()
    } else if m.silent {
        format!(
            "the last {:.1} s are silent (peak {:.1} dBFS)",
            m.window_seconds, m.peak_dbfs
        )
    } else {
        format!(
            "the last {:.1} s play at {:.1} dBFS RMS, peak {:.1} dBFS",
            m.window_seconds, m.rms_dbfs, m.peak_dbfs
        )
    };
    format!(
        "{}: {:.1} s of sound ({} Hz, {} channels); {level}",
        path.display(),
        m.seconds,
        m.sample_rate,
        m.channels
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV header as QEMU writes it while the file is open: both sizes 0.
    fn qemu_wav(samples: &[i16]) -> Vec<u8> {
        let mut out = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(44_100u32.to_le_bytes());
        out.extend((44_100u32 * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data\0\0\0\0");
        for s in samples {
            out.extend(s.to_le_bytes());
        }
        out
    }

    #[test]
    fn an_open_recording_is_measured_to_its_end_and_its_newest_stretch_judged() {
        // One second of silence, then half a second of a loud square wave.
        let mut samples = vec![0i16; 44_100 * 2];
        samples.extend((0..44_100).map(|i| if i % 100 < 50 { 16_000 } else { -16_000 }));
        let wav = qemu_wav(&samples);
        let loud = measure(&wav, 0.5).unwrap();
        assert!((loud.seconds - 1.5).abs() < 1e-9, "{loud:?}");
        assert!(!loud.silent, "{loud:?}");
        assert!((loud.peak_dbfs - 20.0 * (16_000f64 / 32_767.0).log10()).abs() < 0.01);
        let whole = measure(&wav, 10.0).unwrap();
        assert!((whole.window_seconds - 1.5).abs() < 1e-9, "{whole:?}");

        let quiet = measure(&qemu_wav(&vec![0i16; 44_100 * 2]), 0.5).unwrap();
        assert!(quiet.silent, "{quiet:?}");
        assert!(describe(Path::new("d.wav"), &quiet).contains("silent"));
    }

    #[test]
    fn anything_else_is_refused_with_what_it_is() {
        assert!(measure(b"not a wav", 1.0).unwrap_err().contains("RIFF"));
        let mut wav = qemu_wav(&[]);
        wav[34] = 8;
        assert!(measure(&wav, 1.0).unwrap_err().contains("8-bit"));
    }
}
