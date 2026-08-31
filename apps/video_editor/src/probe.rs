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
//!   * **Matroska / `WebM`** (EBML): duration from the `Info` segment's
//!     `Duration` × `TimestampScale`, pixel dimensions from the first video
//!     `TrackEntry`, and the kind from the track types present.
//!   * **MP3** (MPEG-1/2/2.5 Layer III): duration from a `Xing`/`Info` VBR
//!     header when present (exact frame count), else a CBR estimate from the
//!     first frame's bitrate over the audio byte length; `ID3v2` tags are skipped.
//!   * **WAV** (RIFF/WAVE): channels / sample-rate / bit-depth from `fmt `, and
//!     the duration from the `data` chunk size over the byte rate.
//!   * **PNG / JPEG**: pixel dimensions from the header (`IHDR` / `SOFn`).
//!
//! Anything we can't parse degrades honestly: the kind is guessed from the
//! extension and the numeric fields stay zero / unknown, rather than fabricated.
//! Under `--features decode` the caller can additionally confirm duration
//! against the real `GStreamer` pipeline, but the container parse here is already
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
///
/// # Errors
///
/// Returns `Err` if `path`'s metadata or contents can't be read (missing
/// file, permission denied, or another OS-level I/O failure).
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
    if is_wav(data)
        && let Some(p) = parse_wav(data)
    {
        return p;
    }
    if is_isobmff(data)
        && let Some(p) = parse_isobmff(data)
    {
        return p;
    }
    if is_ebml(data)
        && let Some(p) = parse_ebml(data)
    {
        return p;
    }
    if looks_like_mp3(data, path)
        && let Some(p) = parse_mp3(data)
    {
        return p;
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
    let duration = if kind == MediaKind::Image {
        DEFAULT_IMAGE_DURATION
    } else {
        0.0
    };
    ProbedMedia {
        kind,
        duration,
        width: 0,
        height: 0,
        fps: 0.0,
        codec: "unknown".into(),
    }
}

// ---- magic-byte sniffers --------------------------------------------------

fn is_png(d: &[u8]) -> bool {
    d.get(0..8) == Some(b"\x89PNG\r\n\x1a\n".as_slice())
}
fn is_jpeg(d: &[u8]) -> bool {
    d.get(0..3) == Some([0xFF, 0xD8, 0xFF].as_slice())
}
fn is_wav(d: &[u8]) -> bool {
    d.get(0..4) == Some(b"RIFF".as_slice()) && d.get(8..12) == Some(b"WAVE".as_slice())
}
fn is_isobmff(d: &[u8]) -> bool {
    // The first box is normally `ftyp`; be lenient and also accept a leading
    // `moov` / `mdat` / `free` / `skip` / `wide` box (some `.mov` files).
    matches!(
        d.get(4..8),
        Some(b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide")
    )
}
fn is_ebml(d: &[u8]) -> bool {
    // Both Matroska and WebM open with the EBML header magic `1A 45 DF A3`.
    d.get(0..4) == Some([0x1A, 0x45, 0xDF, 0xA3].as_slice())
}
/// Whether `d` (with `path`'s extension as a hint) is an MP3 stream: an `ID3v2`
/// tag, or a raw MPEG audio frame sync in the first bytes. Extension-only files
/// still fall through to the frame-sync scan, so a mislabelled file is rejected
/// rather than reported with fabricated numbers.
fn looks_like_mp3(d: &[u8], path: &Path) -> bool {
    if d.get(0..3) == Some(b"ID3".as_slice()) {
        return true;
    }
    if d.first() == Some(&0xFF) && d.get(1).is_some_and(|b| (b & 0xE0) == 0xE0) {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .eq_ignore_ascii_case("mp3");
    // For a .mp3 without a leading tag/sync, only accept if a sync appears
    // within the first few KB (some files have leading junk).
    ext && d
        .iter()
        .take(8192)
        .enumerate()
        .any(|(i, &b)| b == 0xFF && d.get(i + 1).is_some_and(|n| (n & 0xE0) == 0xE0))
}

// ---- PNG / JPEG dimensions ------------------------------------------------

/// Big-endian `u32` at `at`, or `None` if the buffer is too short.
///
/// These take an offset rather than a pre-sliced buffer because `&d[a..b]` is
/// itself a panic on a short read, and every caller here is parsing a file the
/// user opened. One bounds check inside beats a hundred at the call sites.
fn be_u32(d: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(d.get(at..end)?.try_into().ok()?))
}
fn be_u16(d: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_be_bytes(d.get(at..end)?.try_into().ok()?))
}
fn be_u64(d: &[u8], at: usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    Some(u64::from_be_bytes(d.get(at..end)?.try_into().ok()?))
}

fn png_dimensions(d: &[u8]) -> Option<(u32, u32)> {
    // IHDR is the first chunk: [len:4][ "IHDR" ][ width:4 ][ height:4 ]... at
    // byte 8. Width/height are big-endian at offsets 16 and 20.
    if d.len() < 24 || d.get(12..16) != Some(b"IHDR".as_slice()) {
        return None;
    }
    Some((be_u32(d, 16)?, be_u32(d, 20)?))
}

fn jpeg_dimensions(d: &[u8]) -> Option<(u32, u32)> {
    // Scan the marker segments for a Start-Of-Frame (SOF0..SOF15, excluding the
    // non-frame markers C4/C8/CC), whose payload holds height then width.
    let mut i = 2usize; // skip SOI (FFD8)
    while i + 9 < d.len() {
        if d.get(i) != Some(&0xFF) {
            i += 1;
            continue;
        }
        let &marker = d.get(i + 1)?;
        // Standalone markers (RSTn, SOI, EOI, TEM) have no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let seg_len = be_u16(d, i + 2)? as usize;
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            // SOF payload: precision(1) height(2) width(2) ...
            let h = u32::from(be_u16(d, i + 5)?);
            let w = u32::from(be_u16(d, i + 7)?);
            return Some((w, h));
        }
        i += 2 + seg_len;
    }
    None
}

// ---- WAV ------------------------------------------------------------------

fn le_u16(d: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(d.get(at..end)?.try_into().ok()?))
}
fn le_u32(d: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(d.get(at..end)?.try_into().ok()?))
}

fn parse_wav(d: &[u8]) -> Option<ProbedMedia> {
    let mut i = 12usize; // past "RIFF" size "WAVE"
    let mut byte_rate = 0u32;
    let mut audio_format = 0u16;
    let mut data_len = 0u32;
    while i + 8 <= d.len() {
        let id = d.get(i..i + 4)?;
        let size = le_u32(d, i + 4)? as usize;
        let body = i + 8;
        match id {
            b"fmt " if body + 16 <= d.len() => {
                audio_format = le_u16(d, body)?;
                byte_rate = le_u32(d, body + 8)?;
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
    let duration = f64::from(data_len) / f64::from(byte_rate);
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
    // Every read is checked and the cursor only moves forward, so a box header
    // claiming a bogus size ends the walk instead of indexing past the buffer.
    while i.saturating_add(8) <= data.len() {
        let Some(size32) = be_u32(data, i).map(u64::from) else {
            break;
        };
        let Some(typ) = data
            .get(i.saturating_add(4)..i.saturating_add(8))
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
        else {
            break;
        };
        let (header, total) = if size32 == 1 {
            let Some(large) = data
                .get(i.saturating_add(8)..i.saturating_add(16))
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(u64::from_be_bytes)
            else {
                break;
            };
            (16usize, large)
        } else if size32 == 0 {
            (8usize, (data.len().saturating_sub(i)) as u64)
        } else {
            (8usize, size32)
        };
        let total = total as usize;
        if total < header || i.saturating_add(total) > data.len() {
            // Truncated / bad size — take the rest as this box's body and stop.
            let body_start = i.saturating_add(header).min(data.len());
            out.push(BoxRef {
                typ,
                body: data.get(body_start..).unwrap_or_default(),
            });
            break;
        }
        let Some(body) = data.get(i.saturating_add(header)..i.saturating_add(total)) else {
            break;
        };
        out.push(BoxRef { typ, body });
        // `total >= header >= 8`, so the cursor always advances and the loop
        // terminates even on a file that claims overlapping boxes.
        i = i.saturating_add(total);
    }
    out
}

/// First child box of `typ` within `data`.
fn child<'a>(data: &'a [u8], typ: [u8; 4]) -> Option<&'a [u8]> {
    iter_boxes(data)
        .into_iter()
        .find(|b| b.typ == typ)
        .map(|b| b.body)
}

/// Follow a nested path of four-CCs to a box body.
fn descend<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut cur = data;
    for step in path {
        cur = child(cur, **step)?;
    }
    Some(cur)
}

fn parse_isobmff(data: &[u8]) -> Option<ProbedMedia> {
    let moov = child(data, *b"moov")?;

    // Movie duration from mvhd.
    let (timescale, duration_units) = child(moov, *b"mvhd").and_then(mvhd_time).unwrap_or((0, 0));
    let duration = if timescale > 0 {
        duration_units as f64 / f64::from(timescale)
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
        let track_codec = stbl.and_then(|s| child(s, *b"stsd")).and_then(stsd_codec);

        if is_video && !saw_video_recorded(width, height, &codec) {
            if let Some((w, h)) = child(trak, *b"tkhd").and_then(tkhd_dims) {
                width = w;
                height = h;
            }
            if let Some(c) = &track_codec {
                c.clone_into(&mut codec);
            }
            // fps ≈ video sample count / movie duration.
            let samples = stbl.and_then(|s| child(s, *b"stsz")).and_then(stsz_count);
            if let (Some(n), true) = (samples, duration > 0.0) {
                fps = f64::from(n) / duration;
            }
        } else if is_audio
            && codec == "unknown"
            && !saw_video
            && let Some(c) = track_codec
        {
            codec = c;
        }
    }

    kind = if saw_video {
        MediaKind::Video
    } else if saw_audio {
        MediaKind::Audio
    } else {
        kind
    };

    Some(ProbedMedia {
        kind,
        duration,
        width,
        height,
        fps,
        codec,
    })
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
        let ts = be_u32(body, 20)?;
        let dur = be_u64(body, 24)?;
        Some((ts, dur))
    } else {
        // 1 + 3 + 4 create + 4 modify = 12, then timescale(4), duration(4)
        if body.len() < 20 {
            return None;
        }
        let ts = be_u32(body, 12)?;
        let dur = u64::from(be_u32(body, 16)?);
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
    let w = be_u32(body, n - 8)? >> 16;
    let h = be_u32(body, n - 4)? >> 16;
    Some((w, h))
}

/// Handler type ("vide" / "soun" / …) from an `hdlr` payload (at offset 8).
fn hdlr_type(body: &[u8]) -> Option<String> {
    Some(String::from_utf8_lossy(body.get(8..12)?).trim().to_string())
}

/// Codec four-CC from an `stsd` payload's first sample entry.
fn stsd_codec(body: &[u8]) -> Option<String> {
    // version+flags(4) entry_count(4) then entry: size(4) format(4).
    let fourcc = body.get(12..16)?;
    Some(String::from_utf8_lossy(fourcc).trim().to_string())
}

/// Sample count from an `stsz` payload (at offset 8).
fn stsz_count(body: &[u8]) -> Option<u32> {
    if body.len() < 12 {
        return None;
    }
    be_u32(body, 8)
}

// ---- Matroska / WebM (EBML) ----------------------------------------------

// The EBML element IDs we care about, in their full (marker-retained) form.
const EBML_SEGMENT: u64 = 0x1853_8067;
const EBML_INFO: u64 = 0x1549_A966;
const EBML_TIMESTAMP_SCALE: u64 = 0x2A_D7_B1;
const EBML_DURATION: u64 = 0x4489;
const EBML_TRACKS: u64 = 0x1654_AE6B;
const EBML_TRACK_ENTRY: u64 = 0xAE;
const EBML_TRACK_TYPE: u64 = 0x83;
const EBML_VIDEO: u64 = 0xE0;
const EBML_PIXEL_WIDTH: u64 = 0xB0;
const EBML_PIXEL_HEIGHT: u64 = 0xBA;

/// Read one EBML variable-length integer at `d[i..]`.
///
/// Returns `(value, byte_len)`. For element IDs (`keep_marker`) the leading
/// length-descriptor bits are retained so the value matches the canonical id
/// constants; for sizes the marker is stripped to yield the true length.
fn read_vint(d: &[u8], i: usize, keep_marker: bool) -> Option<(u64, usize)> {
    let first = *d.get(i)?;
    if first == 0 {
        return None; // lengths beyond 8 bytes are not needed here
    }
    let len = first.leading_zeros() as usize + 1; // 1..=8
    let end = i.checked_add(len)?;
    if len > 8 || end > d.len() {
        return None;
    }
    let mut value: u64 = if keep_marker {
        u64::from(first)
    } else {
        // `len == 8` when the first byte is 0x01, and `0xFFu8 >> 8` is a shift
        // overflow rather than 0. At that length the first byte carries only
        // the marker, so no value bits survive the mask.
        let mask = if len >= 8 { 0 } else { 0xFFu8 >> len };
        u64::from(first & mask)
    };
    for k in 1..len {
        let byte = *d.get(i.saturating_add(k))?;
        value = (value << 8) | u64::from(byte);
    }
    Some((value, len))
}

/// The direct child elements of an EBML container `d`, as `(id, data)` pairs.
/// Tolerates truncation and the "unknown size" vint (all size bits set), which
/// a streamed Segment uses — its data then runs to the end of the buffer.
fn ebml_children(d: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < d.len() {
        let Some((id, id_len)) = read_vint(d, i, true) else {
            break;
        };
        let Some((size, sz_len)) = read_vint(d, i + id_len, false) else {
            break;
        };
        let data_start = i + id_len + sz_len;
        if data_start > d.len() {
            break;
        }
        // Unknown size: every value bit set for the vint's width.
        let unknown = size == (1u64 << (7 * sz_len as u64)) - 1;
        let data_end = if unknown || data_start + size as usize > d.len() {
            d.len()
        } else {
            data_start + size as usize
        };
        let Some(slice) = d.get(data_start..data_end) else {
            break;
        };
        out.push((id, slice));
        i = data_end;
    }
    out
}

fn ebml_find(d: &[u8], id: u64) -> Option<&[u8]> {
    ebml_children(d)
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, x)| x)
}

/// Big-endian unsigned integer from an EBML element payload (1..8 bytes).
fn ebml_uint(d: &[u8]) -> u64 {
    d.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// IEEE float from an EBML element payload (4 or 8 bytes; 0 otherwise).
fn ebml_float(d: &[u8]) -> Option<f64> {
    match d.len() {
        4 => Some(f64::from(f32::from_be_bytes(*d.first_chunk::<4>()?))),
        8 => Some(f64::from_be_bytes(*d.first_chunk::<8>()?)),
        _ => None,
    }
}

fn parse_ebml(d: &[u8]) -> Option<ProbedMedia> {
    // The top level holds the EBML header then the Segment.
    let segment = ebml_find(d, EBML_SEGMENT)?;

    // Duration lives in Info as `Duration` (a float, in TimestampScale units).
    let info = ebml_find(segment, EBML_INFO);
    let timescale = info
        .and_then(|i| ebml_find(i, EBML_TIMESTAMP_SCALE))
        .map(ebml_uint)
        .filter(|&t| t > 0)
        .unwrap_or(1_000_000); // Matroska default: 1 ms in nanoseconds
    let duration = info
        .and_then(|i| ebml_find(i, EBML_DURATION))
        .and_then(ebml_float)
        .map_or(0.0, |units| units * timescale as f64 / 1e9);

    // Kind + dimensions from the first video/audio track entry.
    let mut saw_video = false;
    let mut saw_audio = false;
    let mut width = 0u32;
    let mut height = 0u32;
    if let Some(tracks) = ebml_find(segment, EBML_TRACKS) {
        for (id, entry) in ebml_children(tracks) {
            if id != EBML_TRACK_ENTRY {
                continue;
            }
            match ebml_find(entry, EBML_TRACK_TYPE).map(ebml_uint) {
                Some(1) => {
                    saw_video = true;
                    if width == 0
                        && let Some(video) = ebml_find(entry, EBML_VIDEO)
                    {
                        width = ebml_find(video, EBML_PIXEL_WIDTH).map_or(0, ebml_uint) as u32;
                        height = ebml_find(video, EBML_PIXEL_HEIGHT).map_or(0, ebml_uint) as u32;
                    }
                }
                Some(2) => saw_audio = true,
                _ => {}
            }
        }
    }
    let kind = if saw_video {
        MediaKind::Video
    } else if saw_audio {
        MediaKind::Audio
    } else {
        MediaKind::Video
    };
    Some(ProbedMedia {
        kind,
        duration,
        width,
        height,
        fps: 0.0,
        codec: "matroska".to_string(),
    })
}

// ---- MP3 (MPEG-1/2/2.5 Layer III) -----------------------------------------

/// Read a 4-byte `ID3v2` syncsafe integer (7 bits per byte).
fn syncsafe(b: &[u8]) -> Option<usize> {
    let &[b0, b1, b2, b3] = b.first_chunk::<4>()?;
    Some(
        ((b0 as usize & 0x7F) << 21)
            | ((b1 as usize & 0x7F) << 14)
            | ((b2 as usize & 0x7F) << 7)
            | (b3 as usize & 0x7F),
    )
}

/// One decoded MPEG audio frame header.
struct Mp3Header {
    /// True for MPEG Version 1 (vs. 2 / 2.5) — selects samples-per-frame.
    mpeg1: bool,
    /// True for a mono channel mode (selects the Xing header offset).
    mono: bool,
    bitrate_bps: u32,
    sample_rate: u32,
    samples_per_frame: u32,
}

// Layer III bitrate tables (kbps), indexed by bitrate_index.
const MP3_BR_V1: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const MP3_BR_V2: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];
// Sample-rate tables by MPEG version, indexed by samplerate_index.
const MP3_SR_V1: [u32; 3] = [44100, 48000, 32000];
const MP3_SR_V2: [u32; 3] = [22050, 24000, 16000];
const MP3_SR_V25: [u32; 3] = [11025, 12000, 8000];

fn parse_mp3_frame_header(d: &[u8]) -> Option<Mp3Header> {
    let &[b0, b1, b2, b3] = d.first_chunk::<4>()?;
    if b0 != 0xFF || (b1 & 0xE0) != 0xE0 {
        return None;
    }
    let version_bits = (b1 >> 3) & 0x03; // 0=2.5,2=2,3=1 (1 reserved)
    let layer_bits = (b1 >> 1) & 0x03; // 1=Layer III
    if version_bits == 1 || layer_bits != 1 {
        return None; // only Layer III is handled
    }
    let mpeg1 = version_bits == 3;
    let bitrate_index = (b2 >> 4) & 0x0F;
    let samplerate_index = (b2 >> 2) & 0x03;
    let padding = ((b2 >> 1) & 0x01) as usize;
    let channel_mode = (b3 >> 6) & 0x03; // 3 = mono
    if bitrate_index == 0 || bitrate_index == 0x0F || samplerate_index == 3 {
        return None; // free-format / reserved
    }
    let br_kbps = (if mpeg1 { MP3_BR_V1 } else { MP3_BR_V2 })
        .get(bitrate_index as usize)
        .copied()
        .unwrap_or(0);
    let sample_rate = match version_bits {
        3 => MP3_SR_V1,
        2 => MP3_SR_V2,
        _ => MP3_SR_V25,
    }
    .get(samplerate_index as usize)
    .copied()
    .unwrap_or(0);
    let samples_per_frame = if mpeg1 { 1152 } else { 576 };
    let bitrate_bps = br_kbps * 1000;
    let _ = padding;
    Some(Mp3Header {
        mpeg1,
        mono: channel_mode == 3,
        bitrate_bps,
        sample_rate,
        samples_per_frame,
    })
}

fn parse_mp3(d: &[u8]) -> Option<ProbedMedia> {
    // Skip an ID3v2 tag if present.
    let mut off = 0usize;
    if d.get(0..3) == Some(b"ID3".as_slice())
        && let Some(sync) = d.get(6..10).and_then(syncsafe)
    {
        off = 10 + sync;
    }
    // Find the first valid frame sync.
    let mut header = None;
    let mut scan = off;
    let limit = d.len().saturating_sub(4);
    while scan <= limit {
        if d.get(scan) == Some(&0xFF)
            && d.get(scan + 1).is_some_and(|b| (b & 0xE0) == 0xE0)
            && let Some(h) = d.get(scan..).and_then(parse_mp3_frame_header)
        {
            header = Some((scan, h));
            break;
        }
        scan += 1;
        if scan - off > 200_000 {
            break; // no frame in a sane window
        }
    }
    let (frame_off, h) = header?;

    // A Xing / Info VBR header sits at a fixed offset into the first frame and
    // carries the exact total frame count — the accurate source of duration.
    let side_info = if h.mpeg1 {
        if h.mono { 17 } else { 32 }
    } else if h.mono {
        9
    } else {
        17
    };
    let xing_at = frame_off + 4 + side_info;
    if xing_at + 12 <= d.len()
        && let Some(tag) = d.get(xing_at..xing_at + 4)
        && (tag == b"Xing" || tag == b"Info")
    {
        let flags = be_u32(d, xing_at + 4)?;
        if flags & 0x1 != 0 {
            let frames = f64::from(be_u32(d, xing_at + 8)?);
            let duration = frames * f64::from(h.samples_per_frame) / f64::from(h.sample_rate);
            return Some(mp3_probe(duration));
        }
    }

    // No VBR header: constant-bitrate estimate over the audio byte length.
    if h.bitrate_bps == 0 {
        return Some(mp3_probe(0.0));
    }
    let audio_bytes = d.len().saturating_sub(frame_off);
    let duration = (audio_bytes as f64 * 8.0) / f64::from(h.bitrate_bps);
    Some(mp3_probe(duration))
}

fn mp3_probe(duration: f64) -> ProbedMedia {
    ProbedMedia {
        kind: MediaKind::Audio,
        duration,
        width: 0,
        height: 0,
        fps: 0.0,
        codec: "mp3".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- box builder for crafting deterministic ISOBMFF test bytes ----
    fn boxed(typ: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&typ);
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
        let mut stsd_body = Vec::new();
        stsd_body.extend_from_slice(&[0, 0, 0, 0]); // ver+flags
        stsd_body.extend_from_slice(&1u32.to_be_bytes()); // entry count
        stsd_body.extend_from_slice(&16u32.to_be_bytes()); // entry size
        stsd_body.extend_from_slice(b"avc1");

        // stsz: ver+flags(4) sample_size(4=0) sample_count(4=150) → fps 25.
        let mut stsz = vec![0u8; 12];
        stsz[8..12].copy_from_slice(&150u32.to_be_bytes());

        let stbl = boxed(
            *b"stbl",
            &[boxed(*b"stsd", &stsd_body), boxed(*b"stsz", &stsz)].concat(),
        );
        let minf = boxed(*b"minf", &stbl);
        let mdia = boxed(*b"mdia", &[boxed(*b"hdlr", &hdlr), minf].concat());
        let trak = boxed(*b"trak", &[boxed(*b"tkhd", &tkhd), mdia].concat());
        let moov = boxed(*b"moov", &[boxed(*b"mvhd", &mvhd), trak].concat());

        let mut file = boxed(*b"ftyp", b"isom\0\0\0\0isom");
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
        fmt.extend_from_slice(&44_100u32.to_le_bytes()); // sample rate
        fmt.extend_from_slice(&176_400u32.to_le_bytes()); // byte rate
        fmt.extend_from_slice(&4u16.to_le_bytes()); // block align
        fmt.extend_from_slice(&16u16.to_le_bytes()); // bits

        let data_bytes = vec![0u8; 176_400]; // exactly 1.0s of audio
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
        let audio = probe_bytes(b"still nonsense", Path::new("track.opus"));
        assert_eq!(audio.kind, MediaKind::Audio);
        assert_eq!(audio.duration, 0.0);
    }

    // ---- EBML (Matroska / WebM) test builders --------------------------

    /// Encode an EBML size as a minimal-length vint (with its marker bit).
    fn ebml_size(n: usize) -> Vec<u8> {
        for len in 1..=8usize {
            let max = (1u64 << (7 * len as u64)) - 1;
            if (n as u64) < max {
                let bytes = (n as u64).to_be_bytes();
                let mut v = bytes[8 - len..].to_vec();
                v[0] |= 1u8 << (8 - len); // length-descriptor marker bit
                return v;
            }
        }
        vec![0x01]
    }
    /// `id` bytes ++ size vint ++ payload — one EBML element.
    fn ebml(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&ebml_size(payload.len()));
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_a_crafted_webm_for_duration_dims_and_kind() {
        // TimestampScale = 1_000_000 ns, Duration = 8000 units → 8.0 s.
        let timescale = ebml(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes()[5..]);
        let duration = ebml(&[0x44, 0x89], &8000.0f64.to_be_bytes());
        let info = ebml(&[0x15, 0x49, 0xA9, 0x66], &[timescale, duration].concat());

        // One video TrackEntry: TrackType=1, Video{ PixelWidth=320, Height=180 }.
        let ttype = ebml(&[0x83], &[1u8]);
        let pw = ebml(&[0xB0], &320u16.to_be_bytes());
        let ph = ebml(&[0xBA], &180u16.to_be_bytes());
        let video = ebml(&[0xE0], &[pw, ph].concat());
        let entry = ebml(&[0xAE], &[ttype, video].concat());
        let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &entry);

        let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, tracks].concat());
        let header = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &[0x42, 0x82]); // minimal EBML header
        let file = [header, segment].concat();

        let p = probe_bytes(&file, Path::new("clip.webm"));
        assert_eq!(p.kind, MediaKind::Video);
        assert!((p.duration - 8.0).abs() < 1e-6, "duration {}", p.duration);
        assert_eq!((p.width, p.height), (320, 180));
    }

    #[test]
    fn ebml_audio_only_reports_audio_kind() {
        let timescale = ebml(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes()[5..]);
        let duration = ebml(&[0x44, 0x89], &3000.0f64.to_be_bytes()); // 3.0 s
        let info = ebml(&[0x15, 0x49, 0xA9, 0x66], &[timescale, duration].concat());
        let ttype = ebml(&[0x83], &[2u8]); // audio
        let entry = ebml(&[0xAE], &ttype);
        let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &entry);
        let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, tracks].concat());
        let header = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &[]);
        let file = [header, segment].concat();
        let p = probe_bytes(&file, Path::new("sound.webm"));
        assert_eq!(p.kind, MediaKind::Audio);
        assert!((p.duration - 3.0).abs() < 1e-6);
    }

    // ---- MP3 --------------------------------------------------------------

    #[test]
    fn parses_mp3_xing_header_for_exact_duration() {
        // MPEG1 Layer III, 128 kbps, 44100 Hz, stereo; Xing with 1000 frames.
        let mut mp3 = vec![0xFF, 0xFB, 0x90, 0x00];
        mp3.extend_from_slice(&[0u8; 32]); // stereo side info
        mp3.extend_from_slice(b"Xing");
        mp3.extend_from_slice(&1u32.to_be_bytes()); // flags: frame count present
        mp3.extend_from_slice(&1000u32.to_be_bytes());
        mp3.extend_from_slice(&[0u8; 64]);
        let p = probe_bytes(&mp3, Path::new("song.mp3"));
        assert_eq!(p.kind, MediaKind::Audio);
        assert_eq!(p.codec, "mp3");
        // 1000 frames × 1152 samples / 44100 Hz ≈ 26.12 s.
        assert!(
            (p.duration - 26.122).abs() < 0.01,
            "duration {}",
            p.duration
        );
    }

    #[test]
    fn estimates_cbr_mp3_duration_and_skips_id3() {
        // ID3v2 tag of 20 bytes, then a 128 kbps frame with no Xing header.
        let mut mp3 = Vec::new();
        mp3.extend_from_slice(b"ID3\x03\x00\x00");
        mp3.extend_from_slice(&[0x00, 0x00, 0x00, 0x14]); // syncsafe size = 20
        mp3.extend_from_slice(&[0u8; 20]); // tag body
        let audio_start = mp3.len();
        mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        mp3.extend_from_slice(&[0x11u8; 32]); // side info (not "Xing")
        mp3.extend_from_slice(&[0x22u8; 4000]); // audio payload
        let audio_bytes = mp3.len() - audio_start;
        let p = probe_bytes(&mp3, Path::new("cbr.mp3"));
        assert_eq!(p.kind, MediaKind::Audio);
        let expected = audio_bytes as f64 * 8.0 / 128_000.0;
        assert!(
            (p.duration - expected).abs() < 0.05,
            "duration {} vs {}",
            p.duration,
            expected
        );
    }

    /// Media files come from the user, so a malformed one must not crash the
    /// editor. Every parser here reads container headers that carry their own
    /// lengths, which is the shape that indexes past a truncated buffer.
    #[test]
    fn no_input_makes_the_probe_panic() {
        let path = std::path::Path::new("probe.mp4");

        // Every truncation of each container's magic prefix.
        let seeds: [&[u8]; 6] = [
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
            b"\x1a\x45\xdf\xa3\x01\x00\x00\x00\x00\x00\x00\x1f",
            b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x01\x00",
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00",
            b"\xff\xfb\x90\x00",
        ];
        for seed in seeds {
            for n in 0..=seed.len() {
                let _ = probe_bytes(seed.get(..n).unwrap_or_default(), path);
            }
        }

        // Length fields claiming more than the buffer holds — an ISOBMFF box
        // size of 0xFFFF_FFFF, and the 64-bit large-size form with nothing
        // behind it.
        let mut huge = b"\x00\x00\x00\x18ftypmp42".to_vec();
        huge.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        huge.extend_from_slice(b"moov");
        let _ = probe_bytes(&huge, path);

        let mut large = b"\x00\x00\x00\x18ftypmp42".to_vec();
        large.extend_from_slice(&[0, 0, 0, 1]);
        large.extend_from_slice(b"moov");
        let _ = probe_bytes(&large, path);

        // Random bytes at a spread of lengths.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for len in [0usize, 1, 4, 8, 16, 64, 128, 512, 4096] {
            let blob: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 32) as u8
                })
                .collect();
            let _ = probe_bytes(&blob, path);
        }
    }
}
