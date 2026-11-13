//! Media metadata read straight from a file's container bytes, with no decoder
//! and no external process: ISOBMFF, Matroska, MP3, WAV, PNG and JPEG, plus the
//! extension for formats this does not parse.
//!
//! A file that cannot be described honestly is refused rather than guessed at:
//! bytes that are positively not media name what they are, an extension that is
//! no media format is unrecognised, and a container this does not parse keeps the
//! kind from its extension with every number unknown.

use std::path::Path;

use crate::model::MediaKind;

/// A still has no intrinsic duration, so it gets a default span on the timeline.
pub const DEFAULT_IMAGE_DURATION: f64 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ProbedMedia {
    pub kind: MediaKind,
    /// Source length in seconds. `0.0` when a container carries none (or an
    /// unknown format); an image gets [`DEFAULT_IMAGE_DURATION`].
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Frames per second from the container's own timing. **`0.0` means unknown**:
    /// a fragmented container carries none, and callers must render unknown as
    /// unknown rather than substitute a plausible rate.
    pub fps: f64,
    pub codec: String,
    /// Whether a sound track was found alongside the picture. `false` is a floor,
    /// not a claim: it means none was found, and a format this probe does not parse
    /// always answers false.
    pub has_audio: bool,
}

/// A refusal, not a degraded guess: a caller must decline the import and say
/// why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Io(String),
    Empty,
    NotMedia {
        detected: &'static str,
    },
    /// No container matched and the extension names no media format.
    Unrecognised,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let words = match self {
            ProbeError::Io(e) => crate::resource::CATALOG.t_args(
                "video-editor-probe-unreadable",
                &[("error", ui::i18n::ArgValue::Text(e.clone()))],
            ),
            ProbeError::Empty => crate::resource::CATALOG.t("video-editor-probe-empty"),
            ProbeError::NotMedia { detected } => crate::resource::CATALOG.t_args(
                "video-editor-probe-not-media",
                &[(
                    "detected",
                    ui::i18n::ArgValue::Text(crate::resource::CATALOG.t(detected)),
                )],
            ),
            ProbeError::Unrecognised => {
                crate::resource::CATALOG.t("video-editor-probe-unrecognised")
            }
        };
        f.write_str(&words)
    }
}

impl std::error::Error for ProbeError {}

/// The whole file for a small clip, a prefix for a large one, dispatched on the
/// magic bytes.
///
/// Fails when the file holds nothing that can honestly be described as media:
/// the caller must refuse the import and show the message rather than add an
/// item with invented metadata.
pub fn probe_file(path: &Path) -> Result<ProbedMedia, ProbeError> {
    // The metadata lives in the header and `moov`, so a prefix suffices.
    const CAP: u64 = 96 * 1024 * 1024;
    let data = crate::files::read_prefix(path, CAP).map_err(|e| ProbeError::Io(e.to_string()))?;
    probe_bytes(&data, path)
}

/// Pure, so it is testable on crafted bytes; `path` is only an extension hint.
/// Every container we parse is tried first, so only bytes matching no media
/// header at all reach the not-media check.
pub fn probe_bytes(data: &[u8], path: &Path) -> Result<ProbedMedia, ProbeError> {
    if data.is_empty() {
        return Err(ProbeError::Empty);
    }
    if let Some(format) = ui::framework::content::image_format(data)
        && let Some(codec) = match format.extension {
            "png" => Some("png"),
            "jpg" => Some("jpeg"),
            _ => None,
        }
    {
        let (w, h) = ui::framework::content::image_size(data).map_or((0, 0), |(_, w, h)| (w, h));
        return Ok(image_probe(w, h, codec));
    }
    if let Some(p) = parse_wav(data) {
        return Ok(p);
    }
    if is_isobmff(data)
        && let Some(p) = parse_isobmff(data)
    {
        return Ok(p);
    }
    if is_ebml(data)
        && let Some(p) = parse_ebml(data)
    {
        return Ok(p);
    }
    if looks_like_mp3(data, path)
        && let Some(p) = parse_mp3(data)
    {
        return Ok(p);
    }
    // Nothing matched a media header: before the extension, check whether the
    // bytes are positively something else.
    if let Some(detected) = non_media_signature(data) {
        return Err(ProbeError::NotMedia { detected });
    }
    fallback_from_extension(path)
}

/// `None` means it could still be a container we do not parse, never a claim
/// that the bytes are media.
fn non_media_signature(d: &[u8]) -> Option<&'static str> {
    const MAGIC: &[(&[u8], &str)] = &[
        (b"%PDF-", "video-editor-file-pdf"),
        (b"PK\x03\x04", "video-editor-file-zip"),
        (b"PK\x05\x06", "video-editor-file-zip"),
        (b"PK\x07\x08", "video-editor-file-zip"),
        (b"\x7FELF", "video-editor-file-elf"),
        (b"MZ", "video-editor-file-windows"),
        (b"Rar!\x1A\x07", "video-editor-file-rar"),
        (b"\x1F\x8B", "video-editor-file-gzip"),
        (b"BZh", "video-editor-file-bzip2"),
        (b"\xFD7zXZ\x00", "video-editor-file-xz"),
        (b"7z\xBC\xAF\x27\x1C", "video-editor-file-7z"),
        (b"SQLite format 3\x00", "video-editor-file-sqlite"),
        (b"{\\rtf", "video-editor-file-rtf"),
        (b"\xD0\xCF\x11\xE0", "video-editor-file-office"),
        (b"\xCA\xFE\xBA\xBE", "video-editor-file-binary"),
        (b"-----BEGIN ", "video-editor-file-pem"),
    ];
    for (magic, name) in MAGIC {
        if d.starts_with(magic) {
            return Some(name);
        }
    }
    // YUV4MPEG2 is real video that opens with ASCII: the text sniff would
    // otherwise call it a text file.
    if d.starts_with(b"YUV4MPEG2") {
        return None;
    }
    textual_kind(d)
}

/// Media containers are binary, so bytes that read as clean text are never one.
fn textual_kind(d: &[u8]) -> Option<&'static str> {
    let d = d.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(d); // UTF-8 BOM
    let head = d.get(..1024).unwrap_or(d);
    if head.is_empty() {
        return None;
    }
    let mut control = 0usize;
    for &b in head {
        if b == 0 {
            return None;
        }
        if b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r' {
            control += 1;
        }
    }
    if control * 100 > head.len() {
        return None; // >1% control bytes: binary
    }
    let mut end = head.len();
    while end > 0
        && head
            .get(..end)
            .is_some_and(|h| std::str::from_utf8(h).is_err())
    {
        end -= 1;
        if head.len() - end > 4 {
            return None; // not a truncated multi-byte char: genuinely not UTF-8
        }
    }
    let text = std::str::from_utf8(head.get(..end)?).ok()?;
    let lower = text.trim_start().to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return Some("video-editor-file-html");
    }
    if lower.starts_with("<?xml") {
        return Some("video-editor-file-xml");
    }
    if lower.starts_with("<svg") {
        return Some("video-editor-file-svg");
    }
    if lower.starts_with("#extm3u") {
        return Some("video-editor-file-playlist");
    }
    if lower.starts_with('{') || lower.starts_with('[') {
        return Some("video-editor-file-json");
    }
    if lower.starts_with("#!") {
        return Some("video-editor-file-script");
    }
    if lower.starts_with('<') {
        return Some("video-editor-file-markup");
    }
    Some("video-editor-file-text")
}

fn image_probe(w: u32, h: u32, codec: &str) -> ProbedMedia {
    ProbedMedia {
        has_audio: false,
        kind: MediaKind::Image,
        duration: DEFAULT_IMAGE_DURATION,
        width: w,
        height: h,
        fps: 0.0,
        codec: codec.to_string(),
    }
}

/// Media we do not parse but which is unquestionably media, so an import of
/// one is honest with unknown numbers rather than refused.
pub(crate) const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "qt", "mkv", "webm", "avi", "flv", "f4v", "wmv", "asf", "mpg", "mpeg",
    "mpe", "m1v", "m2v", "ts", "m2ts", "mts", "ogv", "3gp", "3g2", "vob", "mxf", "divx", "rm",
    "rmvb", "y4m",
];
pub(crate) const AUDIO_EXTS: &[&str] = &[
    "wav", "wave", "mp3", "flac", "aac", "ogg", "oga", "opus", "m4a", "m4b", "wma", "aiff", "aif",
    "aifc", "ape", "mka", "amr", "au", "caf", "dsf", "wv", "ac3", "dts",
];
pub(crate) const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "gif", "webp", "bmp", "tif", "tiff", "avif", "heic", "heif",
    "ico", "jxl", "tga",
];

/// Trusts the extension for the kind and leaves every number unknown. An
/// extension that names no media format is a refusal: guessing `Video` for any
/// file produced dead bin items.
fn fallback_from_extension(path: &Path) -> Result<ProbedMedia, ProbeError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let e = ext.as_str();
    let kind = if AUDIO_EXTS.contains(&e) {
        MediaKind::Audio
    } else if IMAGE_EXTS.contains(&e) {
        MediaKind::Image
    } else if VIDEO_EXTS.contains(&e) {
        MediaKind::Video
    } else {
        return Err(ProbeError::Unrecognised);
    };
    let duration = if kind == MediaKind::Image {
        DEFAULT_IMAGE_DURATION
    } else {
        0.0
    };
    Ok(ProbedMedia {
        kind,
        duration,
        width: 0,
        height: 0,
        fps: 0.0,
        codec: "unknown".into(),
        // The header was never parsed here, so no sound track was found: a floor,
        // not a claim.
        has_audio: false,
    })
}

fn is_wav(d: &[u8]) -> bool {
    d.starts_with(b"RIFF") && d.get(8..12) == Some(b"WAVE".as_slice())
}
fn is_isobmff(d: &[u8]) -> bool {
    matches!(
        d.get(4..8),
        Some(b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide")
    )
}
fn is_ebml(d: &[u8]) -> bool {
    d.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}
/// An `ID3v2` tag or a raw frame sync, so a mislabelled file is refused
/// rather than given invented numbers.
fn looks_like_mp3(d: &[u8], path: &Path) -> bool {
    if d.starts_with(b"ID3") {
        return true;
    }
    if let [0xFF, second, ..] = d
        && (second & 0xE0) == 0xE0
    {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .eq_ignore_ascii_case("mp3");
    ext && d
        .iter()
        .take(8192)
        .enumerate()
        .any(|(i, &b)| b == 0xFF && d.get(i + 1).is_some_and(|n| (n & 0xE0) == 0xE0))
}

/// An offset rather than a pre-sliced buffer: `&d[a..b]` is itself a panic on
/// a short read, and every caller here parses a file the user opened.
fn be_u32(d: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(d.get(at..end)?.try_into().ok()?))
}

fn le_u16(d: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(d.get(at..end)?.try_into().ok()?))
}
fn le_u32(d: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(d.get(at..end)?.try_into().ok()?))
}

pub(crate) struct WavLayout {
    pub format: u16,
    pub channels: u16,
    pub bits: u16,
    pub byte_rate: u32,
    pub data: Option<(usize, usize)>,
}

pub(crate) fn wav_layout(d: &[u8]) -> Option<WavLayout> {
    if !is_wav(d) {
        return None;
    }
    let mut layout = WavLayout {
        format: 0,
        channels: 1,
        bits: 16,
        byte_rate: 0,
        data: None,
    };
    let mut i = 12usize; // past "RIFF" size "WAVE"
    while let Some(id) = d.get(i..i + 4) {
        let Some(size) = le_u32(d, i + 4) else {
            break;
        };
        let size = size as usize;
        let body = i + 8;
        match id {
            b"fmt " if body + 16 <= d.len() => {
                layout.format = le_u16(d, body)?;
                layout.channels = le_u16(d, body + 2)?.max(1);
                layout.byte_rate = le_u32(d, body + 8)?;
                layout.bits = le_u16(d, body + 14)?;
            }
            b"data" => layout.data = Some((body, size)),
            _ => {}
        }
        i = body + size + (size & 1);
    }
    Some(layout)
}

fn parse_wav(d: &[u8]) -> Option<ProbedMedia> {
    let layout = wav_layout(d)?;
    let (byte_rate, audio_format) = (layout.byte_rate, layout.format);
    let data_len = layout.data.map_or(0, |(_, size)| size as u32);
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
        // A WAV is sound outright; `has_audio` marks a picture that carries sound.
        has_audio: false,
    })
}

struct BoxRef<'a> {
    typ: [u8; 4],
    body: &'a [u8],
}

fn iter_boxes(data: &[u8]) -> Vec<BoxRef<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    // Every read is checked and the cursor only moves forward, so a bogus size
    // ends the walk instead of indexing past the buffer.
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
        i = i.saturating_add(total);
    }
    out
}

fn child(data: &[u8], typ: [u8; 4]) -> Option<&[u8]> {
    iter_boxes(data)
        .into_iter()
        .find(|b| b.typ == typ)
        .map(|b| b.body)
}

fn descend<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut cur = data;
    for step in path {
        cur = child(cur, **step)?;
    }
    Some(cur)
}

fn parse_isobmff(data: &[u8]) -> Option<ProbedMedia> {
    let moov = child(data, *b"moov")?;

    let (timescale, duration_units) = child(moov, *b"mvhd")
        .and_then(timescale_and_duration)
        .unwrap_or((0, 0));
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
                codec.clone_from(c);
            }
            // The track's own sample deltas over its own media timescale: 30000/1001
            // comes out as 29.97, per track, so an edit list cannot skew it. A
            // fragmented MP4 carries no sample table and leaves fps unknown.
            let media_timescale = descend(trak, &[b"mdia", b"mdhd"])
                .and_then(timescale_and_duration)
                .map_or(0, |(ts, _)| ts);
            if let Some(rate) = stbl
                .and_then(|s| child(s, *b"stts"))
                .and_then(|stts| stts_frame_rate(stts, media_timescale))
            {
                fps = rate;
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
        has_audio: saw_audio,
    })
}

fn saw_video_recorded(w: u32, h: u32, codec: &str) -> bool {
    w != 0 && h != 0 && codec != "unknown"
}

// `mvhd` and `mdhd` share this prefix layout: `mvhd`'s timescale times the
/// movie, `mdhd`'s times one track's samples, which frame timing is measured
/// against.
fn timescale_and_duration(body: &[u8]) -> Option<(u32, u64)> {
    let version = *body.first()?;
    if version == 1 {
        if body.len() < 32 {
            return None;
        }
        let ts = be_u32(body, 20)?;
        let dur = u64::from_be_bytes(body.get(24..32)?.try_into().ok()?);
        Some((ts, dur))
    } else {
        if body.len() < 20 {
            return None;
        }
        let ts = be_u32(body, 12)?;
        let dur = u64::from(be_u32(body, 16)?);
        Some((ts, dur))
    }
}

fn tkhd_dims(body: &[u8]) -> Option<(u32, u32)> {
    if body.len() < 8 {
        return None;
    }
    let n = body.len();
    let w = be_u32(body, n - 8)? >> 16;
    let h = be_u32(body, n - 4)? >> 16;
    Some((w, h))
}

fn hdlr_type(body: &[u8]) -> Option<String> {
    if body.len() < 12 {
        return None;
    }
    Some(String::from_utf8_lossy(body.get(8..12)?).trim().to_string())
}

fn stsd_codec(body: &[u8]) -> Option<String> {
    // version+flags(4) entry_count(4) then entry: size(4) format(4).
    if body.len() < 16 {
        return None;
    }
    let fourcc = body.get(12..16)?;
    Some(String::from_utf8_lossy(fourcc).trim().to_string())
}

/// The average over the table's `(sample_count, sample_delta)` pairs: exact for
/// constant-rate video, the true average for variable. `None` when the table is
/// absent or empty, which is the honest "the container does not say".
fn stts_frame_rate(body: &[u8], timescale: u32) -> Option<f64> {
    if timescale == 0 || body.len() < 8 {
        return None;
    }
    let entries = be_u32(body, 4)? as usize;
    let mut samples: u64 = 0;
    let mut units: u64 = 0;
    for e in 0..entries {
        let off = 8 + e * 8;
        let (Some(count), Some(delta)) = (be_u32(body, off), be_u32(body, off + 4)) else {
            break;
        };
        samples = samples.saturating_add(u64::from(count));
        units = units.saturating_add(u64::from(count).saturating_mul(u64::from(delta)));
    }
    if samples == 0 || units == 0 {
        return None;
    }
    Some(samples as f64 * f64::from(timescale) / units as f64)
}

const EBML_SEGMENT: u64 = 0x1853_8067;
const EBML_INFO: u64 = 0x1549_A966;
const EBML_TIMESTAMP_SCALE: u64 = 0x2A_D7B1;
const EBML_DURATION: u64 = 0x4489;
const EBML_TRACKS: u64 = 0x1654_AE6B;
const EBML_TRACK_ENTRY: u64 = 0xAE;
const EBML_TRACK_TYPE: u64 = 0x83;
const EBML_VIDEO: u64 = 0xE0;
const EBML_PIXEL_WIDTH: u64 = 0xB0;
const EBML_PIXEL_HEIGHT: u64 = 0xBA;
const EBML_DEFAULT_DURATION: u64 = 0x23_E383;

/// `(value, byte_len)`. For an id the leading length-descriptor bits are kept so
/// the value matches the canonical constants; for a size they are stripped.
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
        let &byte = d.get(i.saturating_add(k))?;
        value = (value << 8) | u64::from(byte);
    }
    Some((value, len))
}

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
        let unknown = size == (1u64 << (7 * sz_len as u64)) - 1;
        let data_end = if unknown || data_start + size as usize > d.len() {
            d.len()
        } else {
            data_start + size as usize
        };
        let Some(data) = d.get(data_start..data_end) else {
            break;
        };
        out.push((id, data));
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

fn ebml_uint(d: &[u8]) -> u64 {
    d.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

fn ebml_float(d: &[u8]) -> Option<f64> {
    if let Ok(b) = <[u8; 4]>::try_from(d) {
        Some(f64::from(f32::from_be_bytes(b)))
    } else if let Ok(b) = <[u8; 8]>::try_from(d) {
        Some(f64::from_be_bytes(b))
    } else {
        None
    }
}

fn parse_ebml(d: &[u8]) -> Option<ProbedMedia> {
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
    let mut fps = 0.0f64;
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
                    // The track's own nanoseconds-per-frame, when it states one.
                    // Absent (the common WebM case) leaves fps unknown at 0.0.
                    if fps == 0.0
                        && let Some(ns) = ebml_find(entry, EBML_DEFAULT_DURATION)
                            .map(ebml_uint)
                            .filter(|&n| n > 0)
                    {
                        fps = 1e9 / ns as f64;
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
        fps,
        codec: "matroska".to_string(),
        has_audio: saw_audio,
    })
}

fn syncsafe(b: [u8; 4]) -> usize {
    b.iter()
        .fold(0usize, |acc, &byte| (acc << 7) | usize::from(byte & 0x7F))
}

struct Mp3Header {
    /// True for MPEG Version 1 (vs. 2 / 2.5) — selects samples-per-frame.
    mpeg1: bool,
    /// True for a mono channel mode (selects the Xing header offset).
    mono: bool,
    bitrate_bps: u32,
    sample_rate: u32,
    samples_per_frame: u32,
}

fn parse_mp3_frame_header(d: &[u8]) -> Option<Mp3Header> {
    // Layer III bitrate tables (kbps), indexed by bitrate_index.
    const BR_V1: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const BR_V2: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    // Sample-rate tables by MPEG version, indexed by samplerate_index.
    const SR_V1: [u32; 3] = [44_100, 48_000, 32_000];
    const SR_V2: [u32; 3] = [22_050, 24_000, 16_000];
    const SR_V25: [u32; 3] = [11_025, 12_000, 8_000];
    let &[b0, b1, b2, b3, ..] = d else {
        return None;
    };
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
    // Both indices are masked to fit their tables, so these lookups always hit.
    let kbps = *if mpeg1 { BR_V1 } else { BR_V2 }.get(usize::from(bitrate_index))?;
    let sample_rate = *match version_bits {
        3 => SR_V1,
        2 => SR_V2,
        _ => SR_V25,
    }
    .get(usize::from(samplerate_index))?;
    let samples_per_frame = if mpeg1 { 1152 } else { 576 };
    let bitrate_bps = kbps * 1000;
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
    let off = if d.starts_with(b"ID3")
        && let Some(size) = d.get(6..10).and_then(|s| <[u8; 4]>::try_from(s).ok())
    {
        10 + syncsafe(size)
    } else {
        0
    };
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
        && d.get(xing_at..xing_at + 4)
            .is_some_and(|tag| tag == b"Xing" || tag == b"Info")
    {
        let flags = be_u32(d, xing_at + 4)?;
        if flags & 0x1 != 0 {
            let frames = f64::from(be_u32(d, xing_at + 8)?);
            let duration = frames * f64::from(h.samples_per_frame) / f64::from(h.sample_rate);
            return Some(mp3_probe(duration));
        }
    }

    if h.bitrate_bps == 0 {
        return Some(mp3_probe(0.0));
    }
    let audio_bytes = d.len().saturating_sub(frame_off);
    let duration = (audio_bytes as f64 * 8.0) / f64::from(h.bitrate_bps);
    Some(mp3_probe(duration))
}

fn mp3_probe(duration: f64) -> ProbedMedia {
    ProbedMedia {
        // A file that is sound outright: `has_audio` marks a picture that also
        // carries sound, so it stays false here.
        has_audio: false,
        kind: MediaKind::Audio,
        duration,
        width: 0,
        height: 0,
        fps: 0.0,
        codec: "mp3".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    fn boxed(typ: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&typ);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    #[expect(
        clippy::similar_names,
        reason = "each binding is named after the ISOBMFF box it holds"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn probe_parses_crafted_mp4_and_wav_headers() {
        {
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

            // stts: ver+flags(4) entry_count(4=1) then (count=150, delta=1001).
            // With an mdhd timescale of 30000 that is exactly 30000/1001 = 29.97.
            let mut stts = Vec::new();
            stts.extend_from_slice(&[0, 0, 0, 0]); // ver+flags
            stts.extend_from_slice(&1u32.to_be_bytes()); // entry count
            stts.extend_from_slice(&150u32.to_be_bytes()); // sample count
            stts.extend_from_slice(&1001u32.to_be_bytes()); // sample delta

            // mdhd: v0, media timescale 30000 (the track's own clock, not the movie's).
            let mut mdhd = vec![0u8; 20];
            mdhd[12..16].copy_from_slice(&30000u32.to_be_bytes());
            mdhd[16..20].copy_from_slice(&150_150u32.to_be_bytes()); // 150 × 1001

            let stbl = boxed(
                *b"stbl",
                &[boxed(*b"stsd", &stsd), boxed(*b"stts", &stts)].concat(),
            );
            let minf = boxed(*b"minf", &stbl);
            let mdia = boxed(
                *b"mdia",
                &[boxed(*b"mdhd", &mdhd), boxed(*b"hdlr", &hdlr), minf].concat(),
            );
            let trak = boxed(*b"trak", &[boxed(*b"tkhd", &tkhd), mdia].concat());
            let moov = boxed(*b"moov", &[boxed(*b"mvhd", &mvhd), trak].concat());

            let mut file = boxed(*b"ftyp", b"isom\0\0\0\0isom");
            file.extend_from_slice(&moov);

            let p = probe_bytes(&file, Path::new("clip.mp4")).expect("a real mp4 probes");
            assert_eq!(p.kind, MediaKind::Video);
            assert!((p.duration - 6.0).abs() < 1e-6, "duration {}", p.duration);
            assert_eq!((p.width, p.height), (320, 180));
            assert_eq!(p.codec, "avc1");
            // The container states 30000/1001; the probe must report exactly that,
            // not the round number a human would guess.
            assert!(
                (p.fps - 30000.0 / 1001.0).abs() < 1e-6,
                "fps {} should be the stated 29.97",
                p.fps
            );
        }
        {
            // A fragmented MP4 carries an empty `stts` in `moov` — the real timing
            // lives in the `moof` boxes. There is nothing to read, so fps must stay
            // unknown rather than being back-computed from the movie duration.
            let mut mvhd = vec![0u8; 20];
            mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
            mvhd[16..20].copy_from_slice(&6000u32.to_be_bytes());
            let mut tkhd = vec![0u8; 84];
            let n = tkhd.len();
            tkhd[n - 8..n - 4].copy_from_slice(&((640u32) << 16).to_be_bytes());
            tkhd[n - 4..n].copy_from_slice(&((360u32) << 16).to_be_bytes());
            let mut hdlr = vec![0u8; 12];
            hdlr[8..12].copy_from_slice(b"vide");
            let mut mdhd = vec![0u8; 20];
            mdhd[12..16].copy_from_slice(&30000u32.to_be_bytes());
            // stts with zero entries — the fragmented case.
            let mut stts = vec![0u8; 8];
            stts[4..8].copy_from_slice(&0u32.to_be_bytes());
            let stbl = boxed(*b"stbl", &boxed(*b"stts", &stts));
            let minf = boxed(*b"minf", &stbl);
            let mdia = boxed(
                *b"mdia",
                &[boxed(*b"mdhd", &mdhd), boxed(*b"hdlr", &hdlr), minf].concat(),
            );
            let trak = boxed(*b"trak", &[boxed(*b"tkhd", &tkhd), mdia].concat());
            let moov = boxed(*b"moov", &[boxed(*b"mvhd", &mvhd), trak].concat());
            let mut file = boxed(*b"ftyp", b"isom\0\0\0\0isom");
            file.extend_from_slice(&moov);

            let p = probe_bytes(&file, Path::new("frag.mp4")).expect("a real mp4 probes");
            assert_eq!((p.width, p.height), (640, 360), "dimensions are still real");
            assert_eq!(
                p.fps, 0.0,
                "no sample timing → unknown, never a stand-in rate"
            );
        }
        {
            // fmt : PCM, 2ch, 44100Hz, 16-bit → byte_rate 176400.
            let mut fmt = Vec::new();
            fmt.extend_from_slice(&1u16.to_le_bytes()); // audio format PCM
            fmt.extend_from_slice(&2u16.to_le_bytes()); // channels
            fmt.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
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

            let p = probe_bytes(&riff, Path::new("a.wav")).expect("a real wav probes");
            assert_eq!(p.kind, MediaKind::Audio);
            assert!((p.duration - 1.0).abs() < 1e-6, "duration {}", p.duration);
            assert_eq!(p.codec, "pcm");
        }
        {
            // Seven bits per byte, most significant first.
            assert_eq!(syncsafe([0x00, 0x00, 0x02, 0x01]), 257);
            assert_eq!(syncsafe([0x7F, 0x7F, 0x7F, 0x7F]), (1 << 28) - 1);
            // The high bit of each byte is not part of the value.
            assert_eq!(syncsafe([0x80, 0x80, 0x80, 0x81]), 1);
            // An EBML float is four or eight bytes and nothing else.
            assert_eq!(ebml_float(&1.5f32.to_be_bytes()), Some(1.5));
            assert_eq!(ebml_float(&2.25f64.to_be_bytes()), Some(2.25));
            assert_eq!(ebml_float(&[0; 3]), None);
            assert_eq!(ebml_float(&[]), None);
            // A frame header shorter than four bytes is no header.
            assert!(parse_mp3_frame_header(&[0xFF, 0xFB, 0x90]).is_none());
            let h = parse_mp3_frame_header(&[0xFF, 0xFB, 0x90, 0x64]).expect("MPEG-1 Layer III");
            assert_eq!((h.bitrate_bps, h.sample_rate), (128_000, 44_100));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn probe_refuses_non_media_and_never_panics() {
        {
            // A cut-short file can end anywhere inside a header. Each sniffer and
            // parser must answer from what is there rather than read past it.
            let mut mp3 = b"ID3\x04\x00\x00\x00\x00\x00\x14".to_vec();
            mp3.extend_from_slice(&[0u8; 20]);
            mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
            mp3.extend_from_slice(&[0u8; 40]);
            mp3.extend_from_slice(b"Xing\x00\x00\x00\x01\x00\x00\x00\x10");
            let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x02\x80\x00\x00\x01\x68";
            let wav = b"RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x02\x00";
            let jpeg = [
                0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0x68, 0x02, 0x80, 0x03,
            ];
            let mkv = [
                0x1A, 0x45, 0xDF, 0xA3, 0x18, 0x53, 0x80, 0x67, 0x84, 0x15, 0x49, 0xA9,
            ];
            let samples: [&[u8]; 5] = [&mp3, png, wav, &jpeg, &mkv];
            for data in samples {
                for n in 0..=data.len() {
                    let _ = probe_bytes(&data[..n], Path::new("clip.mp3"));
                }
            }
        }
        {
            // The failing case: a download that returned the web page instead of the file,
            // saved under the media name it asked for. The leading whitespace is why a
            // naive prefix check misses it.
            let html =
                b"\n\n<!DOCTYPE html>\n<html lang=\"en\" class=\"fixed_navbar\">\n\t<head>\n\
\t\t<title>\n\t\tEverything wrong with the world is in this video\n\t\t</title>\n\
\t</head>\n\t<body></body>\n</html>";
            let err = probe_bytes(
                html,
                Path::new("redlib_everything_wrong_with_the_world_CMAF_480.mp4"),
            )
            .expect_err("an HTML page is not a video, whatever it is named");
            assert_eq!(
                err,
                ProbeError::NotMedia {
                    detected: "video-editor-file-html"
                }
            );
            assert!(
                err.to_string().contains("HTML page"),
                "the refusal names what the file really is: {err}"
            );
        }
        {
            // Through the production entry point the importer calls.
            let f = std::env::temp_dir().join(format!("tilo-ve-fake-{}.mp4", std::process::id()));
            std::fs::write(&f, b"<!DOCTYPE html><html><body>nope</body></html>").unwrap();
            let err = probe_file(&f).expect_err("probe_file refuses an HTML page named .mp4");
            assert_eq!(
                err,
                ProbeError::NotMedia {
                    detected: "video-editor-file-html"
                }
            );
            std::fs::remove_file(&f).ok();
        }
        {
            for (bytes, name, detected) in [
                (
                    &b"%PDF-1.7\n%\xE2\xE3\xCF\xD3"[..],
                    "paper.mov",
                    "video-editor-file-pdf",
                ),
                (
                    &b"PK\x03\x04\x14\x00"[..],
                    "bundle.mp4",
                    "video-editor-file-zip",
                ),
                (
                    &b"\x7FELF\x02\x01\x01"[..],
                    "clip.mkv",
                    "video-editor-file-elf",
                ),
                (
                    &b"{\"error\": \"not found\"}"[..],
                    "reply.mp4",
                    "video-editor-file-json",
                ),
                (
                    &b"1\n00:00:01,000 --> 00:00:04,000\nhello\n"[..],
                    "subs.mp4",
                    "video-editor-file-text",
                ),
                (
                    &b"#EXTM3U\n#EXT-X-VERSION:3\n"[..],
                    "stream.mp4",
                    "video-editor-file-playlist",
                ),
            ] {
                match probe_bytes(bytes, Path::new(name)) {
                    Err(ProbeError::NotMedia { detected: d }) => {
                        assert_eq!(d, detected, "{name} must be named for what it is");
                    }
                    other => panic!("{name} must be refused as not-media, got {other:?}"),
                }
            }
        }
        {
            assert_eq!(
                probe_bytes(b"", Path::new("empty.mp4")),
                Err(ProbeError::Empty)
            );
        }
        {
            // Arbitrary binary with no media extension: say nothing rather than call it a
            // video.
            let err = probe_bytes(&[0x00, 0x11, 0x22, 0xFE, 0xED, 0x00], Path::new("blob.dat"))
                .expect_err("an unknown extension is not a video");
            assert_eq!(err, ProbeError::Unrecognised);
        }
        // Media files come from the user, so a malformed one must not crash the editor.
        {
            let path = std::path::Path::new("probe.mp4");

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

            // Length fields claiming more than the buffer holds.
            let mut huge = b"\x00\x00\x00\x18ftypmp42".to_vec();
            huge.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            huge.extend_from_slice(b"moov");
            let _ = probe_bytes(&huge, path);

            let mut large = b"\x00\x00\x00\x18ftypmp42".to_vec();
            large.extend_from_slice(&[0, 0, 0, 1]);
            large.extend_from_slice(b"moov");
            let _ = probe_bytes(&large, path);

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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn probe_parses_image_webm_and_mp3_headers() {
        {
            let mut png = Vec::new();
            png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
            png.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
            png.extend_from_slice(b"IHDR");
            png.extend_from_slice(&640u32.to_be_bytes());
            png.extend_from_slice(&360u32.to_be_bytes());
            png.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, colour type, ...
            let p = probe_bytes(&png, Path::new("logo.png")).expect("a real png probes");
            assert_eq!(p.kind, MediaKind::Image);
            assert_eq!((p.width, p.height), (640, 360));
            assert!((p.duration - DEFAULT_IMAGE_DURATION).abs() < 1e-9);
        }
        {
            // SOI, then a SOF0 marker with height 480, width 720.
            let mut jpg = vec![0xFF, 0xD8, 0xFF];
            jpg[2] = 0xFF;
            jpg.push(0xC0); // SOF0
            jpg.extend_from_slice(&(11u16).to_be_bytes()); // segment length
            jpg.push(8); // precision
            jpg.extend_from_slice(&(480u16).to_be_bytes()); // height
            jpg.extend_from_slice(&(720u16).to_be_bytes()); // width
            jpg.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
            let p = probe_bytes(&jpg, Path::new("frame.jpg")).expect("a real jpeg probes");
            assert_eq!(p.kind, MediaKind::Image);
            assert_eq!((p.width, p.height), (720, 480));
        }
        {
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

            let p = probe_bytes(&file, Path::new("clip.webm")).expect("a real webm probes");
            assert_eq!(p.kind, MediaKind::Video);
            assert!((p.duration - 8.0).abs() < 1e-6, "duration {}", p.duration);
            assert_eq!((p.width, p.height), (320, 180));
        }
        {
            let timescale = ebml(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes()[5..]);
            let duration = ebml(&[0x44, 0x89], &3000.0f64.to_be_bytes()); // 3.0 s
            let info = ebml(&[0x15, 0x49, 0xA9, 0x66], &[timescale, duration].concat());
            let ttype = ebml(&[0x83], &[2u8]); // audio
            let entry = ebml(&[0xAE], &ttype);
            let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &entry);
            let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, tracks].concat());
            let header = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &[]);
            let file = [header, segment].concat();
            let p = probe_bytes(&file, Path::new("sound.webm")).expect("a real webm probes");
            assert_eq!(p.kind, MediaKind::Audio);
            assert!((p.duration - 3.0).abs() < 1e-6);
        }
        {
            // MPEG1 Layer III, 128 kbps, 44100 Hz, stereo; Xing with 1000 frames.
            let mut mp3 = vec![0xFF, 0xFB, 0x90, 0x00];
            mp3.extend_from_slice(&[0u8; 32]); // stereo side info
            mp3.extend_from_slice(b"Xing");
            mp3.extend_from_slice(&1u32.to_be_bytes()); // flags: frame count present
            mp3.extend_from_slice(&1000u32.to_be_bytes());
            mp3.extend_from_slice(&[0u8; 64]);
            let p = probe_bytes(&mp3, Path::new("song.mp3")).expect("a real mp3 probes");
            assert_eq!(p.kind, MediaKind::Audio);
            assert_eq!(p.codec, "mp3");
            // 1000 frames × 1152 samples / 44100 Hz ≈ 26.12 s.
            assert!(
                (p.duration - 26.122).abs() < 0.01,
                "duration {}",
                p.duration
            );
        }
        {
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
            let p = probe_bytes(&mp3, Path::new("cbr.mp3")).expect("a real mp3 probes");
            assert_eq!(p.kind, MediaKind::Audio);
            let expected = audio_bytes as f64 * 8.0 / 128_000.0;
            assert!(
                (p.duration - expected).abs() < 0.05,
                "duration {} vs {}",
                p.duration,
                expected
            );
        }
        {
            // AVI and FLAC are genuine media we have no parser for: right kind, no
            // invented numbers.
            let avi = probe_bytes(
                &[
                    b'R', b'I', b'F', b'F', 0x10, 0x00, 0x00, 0x00, b'A', b'V', b'I', b' ', 0x00,
                    0x01,
                ],
                Path::new("legacy.avi"),
            )
            .expect("an AVI is still importable");
            assert_eq!(avi.kind, MediaKind::Video);
            assert_eq!((avi.width, avi.height), (0, 0));
            assert_eq!(avi.duration, 0.0);
            assert_eq!(avi.codec, "unknown");

            let flac = probe_bytes(b"fLaC\x00\x00\x00\x22\x10\x00", Path::new("track.flac"))
                .expect("a FLAC is still importable");
            assert_eq!(flac.kind, MediaKind::Audio);
            assert_eq!(flac.duration, 0.0);

            // YUV4MPEG2 opens with a line of ASCII and is still real video.
            let y4m = probe_bytes(
                b"YUV4MPEG2 W640 H480 F30:1 Ip A1:1\nFRAME\n",
                Path::new("raw.y4m"),
            )
            .expect("a y4m header is not a text file");
            assert_eq!(y4m.kind, MediaKind::Video);
        }
    }

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
    fn ebml(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&ebml_size(payload.len()));
        v.extend_from_slice(payload);
        v
    }
}
