//! Real, dependency-free media probing.
//!
//! Import turns a picked file into a [`crate::model::MediaItem`], which needs
//! real metadata — duration, dimensions, codec, kind. This module reads that
//! straight from the container bytes, with no decoder and no external process,
//! so it produces **real** metadata on the default (hermetic) build:
//!
//!   * **ISOBMFF** (`.mp4` / `.mov` / `.m4v` / `.m4a`): duration from `mvhd`,
//!     pixel dimensions from the video track's `tkhd`, the codec four-CC from
//!     `stsd`, the kind from `hdlr`, and the frame rate from the video track's
//!     sample count over the movie duration.
//!   * **WAV** (RIFF/WAVE): channels / sample-rate / bit-depth from `fmt `, and
//!     the duration from the `data` chunk size over the byte rate.
//!   * **PNG / JPEG**: pixel dimensions from the header (`IHDR` / `SOFn`).
//!
//! Anything we can't parse degrades honestly: the kind is guessed from the
//! extension and the numeric fields stay zero / unknown, rather than fabricated.
//! Under `--features decode` the caller can additionally confirm duration
//! against the real GStreamer pipeline, but the container parse here is already
//! real and is what the default build relies on.

use std::path::Path;

use crate::model::MediaKind;

/// A still image has no intrinsic duration; give it a sensible default span on
/// the timeline (matching the common NLE convention) so it can be placed.
pub const DEFAULT_IMAGE_DURATION: f64 = 5.0;

/// Real metadata read from a file's container, ready to become a `MediaItem`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbedMedia {
    pub kind: MediaKind,
    /// Source length in seconds. `0.0` when a container carries none (or an
    /// unknown format); an image gets [`DEFAULT_IMAGE_DURATION`].
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Container/codec tag (four-CC or format name), or `"unknown"`.
    pub codec: String,
}

/// Probe a file on disk into real [`ProbedMedia`]. Reads the bytes (the whole
/// file for the small clips this handles; a large import is capped) and
/// dispatches on the magic bytes.
pub fn probe_file(path: &Path) -> std::io::Result<ProbedMedia> {
    // Read the file to parse its container header. Cap very large files: the
    // metadata we need lives in the header + `moov`, and a faststart MP4 puts
    // `moov` up front, so the first slice suffices for the common case.
    const CAP: u64 = 96 * 1024 * 1024;
    let len = std::fs::metadata(path)?.len();
    let data = if len <= CAP {
        std::fs::read(path)?
    } else {
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let mut buf = vec![0u8; CAP as usize];
        let n = f.read(&mut buf)?;
        buf.truncate(n);
        buf
    };
    Ok(probe_bytes(&data, path))
}

/// Probe already-read `data`. Pure, so it is unit-testable on crafted bytes.
/// `path` is used only as an extension hint when the magic bytes are unknown.
pub fn probe_bytes(data: &[u8], path: &Path) -> ProbedMedia {
    if is_png(data) {
        let (w, h) = png_dimensions(data).unwrap_or((0, 0));
        return image_probe(w, h, "png");
    }
    if is_jpeg(data) {
        let (w, h) = jpeg_dimensions(data).unwrap_or((0, 0));
        return image_probe(w, h, "jpeg");
    }
    if is_wav(data) {
        if let Some(p) = parse_wav(data) {
            return p;
        }
    }
    if is_isobmff(data) {
        if let Some(p) = parse_isobmff(data) {
            return p;
        }
    }
    // Unknown container: honest fallback from the extension, no invented metadata.
    fallback_from_extension(path)
}

fn image_probe(w: u32, h: u32, codec: &str) -> ProbedMedia {
    ProbedMedia {
        kind: MediaKind::Image,
        duration: DEFAULT_IMAGE_DURATION,
        width: w,
        height: h,
        fps: 0.0,
        codec: codec.to_string(),
    }
}

fn fallback_from_extension(path: &Path) -> ProbedMedia {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let kind = match ext.as_str() {
        "wav" | "mp3" | "flac" | "aac" | "ogg" | "opus" | "m4a" => MediaKind::Audio,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" => MediaKind::Image,
        _ => MediaKind::Video,
    };
    let duration = if kind == MediaKind::Image { DEFAULT_IMAGE_DURATION } else { 0.0 };
    ProbedMedia { kind, duration, width: 0, height: 0, fps: 0.0, codec: "unknown".into() }
}

// ---- magic-byte sniffers --------------------------------------------------

fn is_png(d: &[u8]) -> bool {
    d.len() >= 8 && &d[0..8] == b"\x89PNG\r\n\x1a\n"
}
fn is_jpeg(d: &[u8]) -> bool {
    d.len() >= 3 && d[0] == 0xFF && d[1] == 0xD8 && d[2] == 0xFF
}
fn is_wav(d: &[u8]) -> bool {
    d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WAVE"
}
fn is_isobmff(d: &[u8]) -> bool {
    // The first box is normally `ftyp`; be lenient and also accept a leading
    // `moov` / `mdat` / `free` / `skip` / `wide` box (some `.mov` files).
    d.len() >= 8
        && matches!(
            &d[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide"
        )
}

// ---- PNG / JPEG dimensions ------------------------------------------------

fn be_u32(d: &[u8]) -> u32 {
    u32::from_be_bytes([d[0], d[1], d[2], d[3]])
}
fn be_u16(d: &[u8]) -> u16 {
    u16::from_be_bytes([d[0], d[1]])
}

fn png_dimensions(d: &[u8]) -> Option<(u32, u32)> {
    // IHDR is the first chunk: [len:4][ "IHDR" ][ width:4 ][ height:4 ]... at
    // byte 8. Width/height are big-endian at offsets 16 and 20.
    if d.len() < 24 || &d[12..16] != b"IHDR" {
        return None;
    }
    Some((be_u32(&d[16..20]), be_u32(&d[20..24])))
}

fn jpeg_dimensions(d: &[u8]) -> Option<(u32, u32)> {
    // Scan the marker segments for a Start-Of-Frame (SOF0..SOF15, excluding the
    // non-frame markers C4/C8/CC), whose payload holds height then width.
    let mut i = 2usize; // skip SOI (FFD8)
    while i + 9 < d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = d[i + 1];
        // Standalone markers (RSTn, SOI, EOI, TEM) have no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let seg_len = be_u16(&d[i + 2..i + 4]) as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC;
        if is_sof {
            // SOF payload: precision(1) height(2) width(2) ...
            let h = be_u16(&d[i + 5..i + 7]) as u32;
            let w = be_u16(&d[i + 7..i + 9]) as u32;
            return Some((w, h));
        }
        i += 2 + seg_len;
    }
    None
}

// ---- WAV ------------------------------------------------------------------

fn le_u16(d: &[u8]) -> u16 {
    u16::from_le_bytes([d[0], d[1]])
}
fn le_u32(d: &[u8]) -> u32 {
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}

fn parse_wav(d: &[u8]) -> Option<ProbedMedia> {
    let mut i = 12usize; // past "RIFF" size "WAVE"
    let mut byte_rate = 0u32;
    let mut audio_format = 0u16;
    let mut data_len = 0u32;
    while i + 8 <= d.len() {
        let id = &d[i..i + 4];
        let size = le_u32(&d[i + 4..i + 8]) as usize;
        let body = i + 8;
        match id {
            b"fmt " if body + 16 <= d.len() => {
                audio_format = le_u16(&d[body..body + 2]);
                byte_rate = le_u32(&d[body + 8..body + 12]);
            }
            b"data" => {
                data_len = size as u32;
            }
            _ => {}
        }
        // Chunks are word-aligned (pad byte when the size is odd).
        i = body + size + (size & 1);
    }
    if byte_rate == 0 {
        return None;
    }
    let duration = data_len as f64 / byte_rate as f64;
    let codec = match audio_format {
        1 => "pcm".to_string(),
        3 => "float".to_string(),
        0xFFFE => "extensible".to_string(),
        other => format!("wav-0x{other:04x}"),
    };
    Some(ProbedMedia {
        kind: MediaKind::Audio,
        duration,
        width: 0,
        height: 0,
        fps: 0.0,
        codec,
    })
}

// ---- ISOBMFF (MP4 / MOV) --------------------------------------------------

/// One box's payload slice and four-CC.
struct BoxRef<'a> {
    typ: [u8; 4],
    body: &'a [u8],
}

/// Iterate the sibling boxes contained in `data`. Tolerates truncation.
fn iter_boxes(data: &[u8]) -> Vec<BoxRef<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let size32 = be_u32(&data[i..i + 4]) as u64;
        let typ = [data[i + 4], data[i + 5], data[i + 6], data[i + 7]];
        let (header, total) = if size32 == 1 {
            if i + 16 > data.len() {
                break;
            }
            let large = u64::from_be_bytes([
                data[i + 8], data[i + 9], data[i + 10], data[i + 11],
                data[i + 12], data[i + 13], data[i + 14], data[i + 15],
            ]);
            (16usize, large)
        } else if size32 == 0 {
            (8usize, (data.len() - i) as u64)
        } else {
            (8usize, size32)
        };
        let total = total as usize;
        if total < header || i + total > data.len() {
            // Truncated / bad size — take the rest as this box's body and stop.
            let body_start = (i + header).min(data.len());
            out.push(BoxRef { typ, body: &data[body_start..] });
            break;
        }
        out.push(BoxRef { typ, body: &data[i + header..i + total] });
        i += total;
    }
    out
}

/// First child box of `typ` within `data`.
fn child<'a>(data: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    iter_boxes(data).into_iter().find(|b| &b.typ == typ).map(|b| b.body)
}

/// Follow a nested path of four-CCs to a box body.
fn descend<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut cur = data;
    for step in path {
        cur = child(cur, step)?;
    }
    Some(cur)
}

fn parse_isobmff(data: &[u8]) -> Option<ProbedMedia> {
    let moov = child(data, b"moov")?;

    // Movie duration from mvhd.
    let (timescale, duration_units) = child(moov, b"mvhd").and_then(mvhd_time).unwrap_or((0, 0));
    let duration = if timescale > 0 {
        duration_units as f64 / timescale as f64
    } else {
        0.0
    };

    // Walk every track; prefer a video track for dims + codec + fps.
    let mut kind = MediaKind::Video;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut fps = 0.0;
    let mut codec = String::from("unknown");
    let mut saw_video = false;
    let mut saw_audio = false;

    for b in iter_boxes(moov) {
        if &b.typ != b"trak" {
            continue;
        }
        let trak = b.body;
        let handler = descend(trak, &[b"mdia", b"hdlr"]).and_then(hdlr_type);
        let is_video = handler.as_deref() == Some("vide");
        let is_audio = handler.as_deref() == Some("soun");
        saw_video |= is_video;
        saw_audio |= is_audio;

        let stbl = descend(trak, &[b"mdia", b"minf", b"stbl"]);
        let track_codec = stbl.and_then(|s| child(s, b"stsd")).and_then(stsd_codec);

        if is_video && !saw_video_recorded(width, height, &codec) {
            if let Some((w, h)) = child(trak, b"tkhd").and_then(tkhd_dims) {
                width = w;
                height = h;
            }
            if let Some(c) = &track_codec {
                codec = c.clone();
            }
            // fps ≈ video sample count / movie duration.
            let samples = stbl.and_then(|s| child(s, b"stsz")).and_then(stsz_count);
            if let (Some(n), true) = (samples, duration > 0.0) {
                fps = n as f64 / duration;
            }
        } else if is_audio && codec == "unknown" && !saw_video {
            if let Some(c) = track_codec {
                codec = c;
            }
        }
    }

    kind = if saw_video {
        MediaKind::Video
    } else if saw_audio {
        MediaKind::Audio
    } else {
        kind
    };

    Some(ProbedMedia { kind, duration, width, height, fps, codec })
}

fn saw_video_recorded(w: u32, h: u32, codec: &str) -> bool {
    w != 0 && h != 0 && codec != "unknown"
}

/// (timescale, duration) from an `mvhd` payload.
fn mvhd_time(body: &[u8]) -> Option<(u32, u64)> {
    let version = *body.first()?;
    if version == 1 {
        // 1 + 3 flags + 8 create + 8 modify = 20, then timescale(4), duration(8)
        if body.len() < 32 {
            return None;
        }
        let ts = be_u32(&body[20..24]);
        let dur = u64::from_be_bytes([
            body[24], body[25], body[26], body[27], body[28], body[29], body[30], body[31],
        ]);
        Some((ts, dur))
    } else {
        // 1 + 3 + 4 create + 4 modify = 12, then timescale(4), duration(4)
        if body.len() < 20 {
            return None;
        }
        let ts = be_u32(&body[12..16]);
        let dur = be_u32(&body[16..20]) as u64;
        Some((ts, dur))
    }
}

/// Track pixel dimensions from a `tkhd` payload. Width/height are the last two
/// 16.16 fixed-point u32s of the payload, regardless of version.
fn tkhd_dims(body: &[u8]) -> Option<(u32, u32)> {
    if body.len() < 8 {
        return None;
    }
    let n = body.len();
    let w = be_u32(&body[n - 8..n - 4]) >> 16;
    let h = be_u32(&body[n - 4..n]) >> 16;
    Some((w, h))
}

/// Handler type ("vide" / "soun" / …) from an `hdlr` payload (at offset 8).
fn hdlr_type(body: &[u8]) -> Option<String> {
    if body.len() < 12 {
        return None;
    }
    Some(String::from_utf8_lossy(&body[8..12]).trim().to_string())
}

/// Codec four-CC from an `stsd` payload's first sample entry.
fn stsd_codec(body: &[u8]) -> Option<String> {
    // version+flags(4) entry_count(4) then entry: size(4) format(4).
    if body.len() < 16 {
        return None;
    }
    let fourcc = &body[12..16];
    Some(String::from_utf8_lossy(fourcc).trim().to_string())
}

/// Sample count from an `stsz` payload (at offset 8).
fn stsz_count(body: &[u8]) -> Option<u32> {
    if body.len() < 12 {
        return None;
    }
    Some(be_u32(&body[8..12]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- box builder for crafting deterministic ISOBMFF test bytes ----
    fn boxed(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_a_crafted_mp4_for_duration_dims_codec_and_fps() {
        // mvhd: v0, timescale 1000, duration 6000 → 6.0s.
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&6000u32.to_be_bytes());

        // tkhd: v0, last 8 bytes are 320.0 and 180.0 in 16.16.
        let mut tkhd = vec![0u8; 84];
        let n = tkhd.len();
        tkhd[n - 8..n - 4].copy_from_slice(&((320u32) << 16).to_be_bytes());
        tkhd[n - 4..n].copy_from_slice(&((180u32) << 16).to_be_bytes());

        // hdlr: version+flags(4) predef(4) handler(4="vide") ...
        let mut hdlr = vec![0u8; 12];
        hdlr[8..12].copy_from_slice(b"vide");

        // stsd: ver+flags(4) count(4=1) entry_size(4) fourcc(4="avc1")
        let mut stsd = Vec::new();
        stsd.extend_from_slice(&[0, 0, 0, 0]); // ver+flags
        stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
        stsd.extend_from_slice(&16u32.to_be_bytes()); // entry size
        stsd.extend_from_slice(b"avc1");

        // stsz: ver+flags(4) sample_size(4=0) sample_count(4=150) → fps 25.
        let mut stsz = vec![0u8; 12];
        stsz[8..12].copy_from_slice(&150u32.to_be_bytes());

        let stbl = boxed(b"stbl", &[boxed(b"stsd", &stsd), boxed(b"stsz", &stsz)].concat());
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &[boxed(b"hdlr", &hdlr), minf].concat());
        let trak = boxed(b"trak", &[boxed(b"tkhd", &tkhd), mdia].concat());
        let moov = boxed(b"moov", &[boxed(b"mvhd", &mvhd), trak].concat());

        let mut file = boxed(b"ftyp", b"isom\0\0\0\0isom");
        file.extend_from_slice(&moov);

        let p = probe_bytes(&file, Path::new("clip.mp4"));
        assert_eq!(p.kind, MediaKind::Video);
        assert!((p.duration - 6.0).abs() < 1e-6, "duration {}", p.duration);
        assert_eq!((p.width, p.height), (320, 180));
        assert_eq!(p.codec, "avc1");
        assert!((p.fps - 25.0).abs() < 1e-6, "fps {}", p.fps);
    }

    #[test]
    fn parses_a_crafted_wav_for_duration_and_kind() {
        // fmt : PCM, 2ch, 44100Hz, 16-bit → byte_rate 176400.
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // audio format PCM
        fmt.extend_from_slice(&2u16.to_le_bytes()); // channels
        fmt.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        fmt.extend_from_slice(&176400u32.to_le_bytes()); // byte rate
        fmt.extend_from_slice(&4u16.to_le_bytes()); // block align
        fmt.extend_from_slice(&16u16.to_le_bytes()); // bits

        let data_bytes = vec![0u8; 176400]; // exactly 1.0s of audio
        let mut riff = Vec::new();
        riff.extend_from_slice(b"RIFF");
        riff.extend_from_slice(&0u32.to_le_bytes()); // riff size (ignored)
        riff.extend_from_slice(b"WAVE");
        riff.extend_from_slice(b"fmt ");
        riff.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        riff.extend_from_slice(&fmt);
        riff.extend_from_slice(b"data");
        riff.extend_from_slice(&(data_bytes.len() as u32).to_le_bytes());
        riff.extend_from_slice(&data_bytes);

        let p = probe_bytes(&riff, Path::new("a.wav"));
        assert_eq!(p.kind, MediaKind::Audio);
        assert!((p.duration - 1.0).abs() < 1e-6, "duration {}", p.duration);
        assert_eq!(p.codec, "pcm");
    }

    #[test]
    fn parses_png_dimensions() {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&360u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, colour type, ...
        let p = probe_bytes(&png, Path::new("logo.png"));
        assert_eq!(p.kind, MediaKind::Image);
        assert_eq!((p.width, p.height), (640, 360));
        assert!((p.duration - DEFAULT_IMAGE_DURATION).abs() < 1e-9);
    }

    #[test]
    fn parses_jpeg_dimensions() {
        // SOI, then a SOF0 marker with height 480, width 720.
        let mut jpg = vec![0xFF, 0xD8, 0xFF];
        jpg[2] = 0xFF;
        jpg.push(0xC0); // SOF0
        jpg.extend_from_slice(&(11u16).to_be_bytes()); // segment length
        jpg.push(8); // precision
        jpg.extend_from_slice(&(480u16).to_be_bytes()); // height
        jpg.extend_from_slice(&(720u16).to_be_bytes()); // width
        jpg.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        let p = probe_bytes(&jpg, Path::new("frame.jpg"));
        assert_eq!(p.kind, MediaKind::Image);
        assert_eq!((p.width, p.height), (720, 480));
    }

    #[test]
    fn unknown_container_degrades_honestly_by_extension() {
        let p = probe_bytes(b"not a real media file at all", Path::new("mystery.mkv"));
        // No fabricated numbers — zero dims, unknown codec, video-ish default.
        assert_eq!((p.width, p.height), (0, 0));
        assert_eq!(p.codec, "unknown");
        let audio = probe_bytes(b"still nonsense", Path::new("track.mp3"));
        assert_eq!(audio.kind, MediaKind::Audio);
        assert_eq!(audio.duration, 0.0);
    }
}
