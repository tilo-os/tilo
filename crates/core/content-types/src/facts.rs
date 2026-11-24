//! Facts a file's own header states, read from its bytes without decoding it: a picture's size,
//! how long a recording plays, how many pages a document has, and the codec and container.

/// How many of a file's first bytes [`facts`] reads at most; a reader that holds more passes this
/// many from the start, and may add as many from the end, where a recording often keeps its index.
pub const FACTS_SCAN_BYTES: usize = 16 * 1024 * 1024;

/// Extensions [`facts`] reads from; keep in step with its match.
const EXTENSIONS: &[&str] = &[
    "png", "gif", "jpg", "jpeg", "wav", "pdf", "mp4", "m4v", "m4a", "mov",
];

/// What a file's header says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaFacts {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub page_count: Option<u32>,
    pub codec: Option<String>,
    pub container: Option<String>,
}

/// Whether [`facts`] can find anything in a file called `name`, so a caller can skip reading it.
#[must_use]
pub fn has_facts(name: &str) -> bool {
    extension(name).is_some_and(|ext| EXTENSIONS.contains(&ext.as_str()))
}

fn extension(name: &str) -> Option<String> {
    name.rsplit('.')
        .next()
        .filter(|ext| *ext != name)
        .map(str::to_ascii_lowercase)
}

/// The facts the file called `name` states in `bytes`, its first [`FACTS_SCAN_BYTES`] (and, for a
/// recording, possibly its last as many after them). `None` when its name and its bytes disagree or
/// its header states nothing.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "header offsets are fixed and guarded by format-specific length checks"
)]
pub fn facts(name: &str, bytes: &[u8]) -> Option<MediaFacts> {
    let ext = extension(name)?;
    let mut value = MediaFacts::default();
    match ext.as_str() {
        "png" | "gif" | "jpg" | "jpeg" => {
            let (format, width, height) = crate::image_size(bytes)?;
            // A JPEG is named `jpeg`, whichever spelling the file uses.
            let found = if format.extension == "jpg" {
                "jpeg"
            } else {
                format.extension
            };
            let named = if ext == "jpg" { "jpeg" } else { ext.as_str() };
            if found != named {
                return None;
            }
            value.width = Some(width);
            value.height = Some(height);
            value.codec = Some(found.into());
            value.container = Some(found.into());
        }
        "wav" => {
            let (duration_ms, codec) = wav(bytes)?;
            value.duration_ms = Some(duration_ms);
            value.codec = Some(codec.into());
            value.container = Some("wav".into());
        }
        "pdf" => {
            if !bytes.starts_with(b"%PDF-") {
                return None;
            }
            let count = bytes
                .windows(11)
                .enumerate()
                .filter(|(at, window)| {
                    *window == b"/Type /Page" && bytes.get(*at + 11) != Some(&b's')
                })
                .count();
            value.page_count = u32::try_from(count).ok().filter(|count| *count > 0);
            value.codec = Some("pdf".into());
            value.container = Some("pdf".into());
        }
        "mp4" | "m4v" | "m4a" | "mov" => {
            if bytes.get(4..8) != Some(b"ftyp") {
                return None;
            }
            value.duration_ms = movie_duration(bytes);
            value.codec = sample_format(bytes);
            value.container = Some(if ext == "mov" { "quicktime" } else { "mp4" }.into());
            if value.duration_ms.is_none() && value.codec.is_none() {
                return None;
            }
        }
        _ => return None,
    }
    Some(value)
}

/// A WAV recording's length and sample encoding, from its `fmt ` and `data` chunks.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::indexing_slicing,
    reason = "RIFF chunk bounds are checked before fixed-width header reads"
)]
fn wav(bytes: &[u8]) -> Option<(u64, &'static str)> {
    if bytes.len() < 12 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut byte_rate = None;
    let mut format = None;
    let mut data_len = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len =
            usize::try_from(u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?)).ok()?;
        let body = at + 8;
        let end = body.checked_add(len)?.min(bytes.len());
        if id == b"fmt " && end >= body + 12 {
            format = Some(u16::from_le_bytes(bytes[body..body + 2].try_into().ok()?));
            byte_rate =
                Some(u32::from_le_bytes(bytes[body + 8..body + 12].try_into().ok()?) as u64);
        } else if id == b"data" {
            data_len = Some(len as u64);
        }
        at = body.checked_add(len)?.checked_add(len & 1)?;
    }
    let rate = byte_rate.filter(|rate| *rate > 0)?;
    let duration = data_len?.checked_mul(1_000)?.checked_div(rate)?;
    Some((
        duration,
        match format? {
            1 => "pcm",
            3 => "float",
            _ => "wav",
        },
    ))
}

fn find(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
    bytes.windows(4).position(|window| window == tag)
}

/// An MP4 or QuickTime movie's length, from its `mvhd` box: the duration in the movie's own time
/// scale. Found by its tag, so a reader that joined a file's head and tail still finds it.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "every read is bounded by `get`"
)]
fn movie_duration(bytes: &[u8]) -> Option<u64> {
    let at = find(bytes, b"mvhd")? + 4;
    let version = *bytes.get(at)?;
    let be32 = |from: usize| -> Option<u64> {
        Some(u64::from(u32::from_be_bytes(
            bytes.get(from..from + 4)?.try_into().ok()?,
        )))
    };
    let (scale, duration) = if version == 1 {
        let scale = be32(at + 4 + 16)?;
        let duration = u64::from_be_bytes(bytes.get(at + 24..at + 32)?.try_into().ok()?);
        (scale, duration)
    } else {
        (be32(at + 4 + 8)?, be32(at + 16)?)
    };
    let ms = duration.checked_mul(1_000)?.checked_div(scale.max(1))?;
    (scale > 0 && ms > 0).then_some(ms)
}

/// The first track's sample format, from its `stsd` box, in the words people know it by.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "every read is bounded by `get`"
)]
fn sample_format(bytes: &[u8]) -> Option<String> {
    let at = find(bytes, b"stsd")? + 4 + 8 + 4;
    let tag = bytes.get(at..at + 4)?;
    Some(
        match tag {
            b"avc1" | b"avc3" => "h264",
            b"hvc1" | b"hev1" => "hevc",
            b"av01" => "av1",
            b"vp09" => "vp9",
            b"mp4a" => "aac",
            b"Opus" => "opus",
            b"apcn" | b"apch" | b"apcs" | b"apco" | b"ap4h" => "prores",
            _ => {
                return std::str::from_utf8(tag)
                    .ok()
                    .map(|tag| tag.trim().to_owned());
            }
        }
        .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_header_states_its_size_length_and_pages() {
        let mut png = vec![0; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&640u32.to_be_bytes());
        png[20..24].copy_from_slice(&480u32.to_be_bytes());
        let p = facts("photo.png", &png).unwrap();
        assert_eq!((p.width, p.height), (Some(640), Some(480)));

        let p = facts("a.pdf", b"%PDF-1.7 /Type /Pages /Type /Page /Type /Page\n").unwrap();
        assert_eq!(p.page_count, Some(2));

        let mut wav = Vec::from(&b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x40\x1f\x00\x00\x01\x00\x08\x00data\x40\x1f\x00\x00"[..]);
        wav.resize(wav.len() + 8_000, 0);
        let p = facts("tone.wav", &wav).unwrap();
        assert_eq!(p.duration_ms, Some(1_000));
        assert_eq!(p.codec.as_deref(), Some("pcm"));
        assert_eq!(p.container.as_deref(), Some("wav"));

        assert!(facts("photo.png", b"not a png").is_none());
    }

    /// A movie of `seconds` at time scale 600, with an H.264 track.
    fn movie(seconds: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\0\0\0\x18ftypisom\0\0\0\0isomavc1");
        bytes.extend_from_slice(b"\0\0\0\x6cmoov\0\0\0\x64mvhd\0\0\0\0");
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&600u32.to_be_bytes());
        bytes.extend_from_slice(&(seconds * 600).to_be_bytes());
        bytes.extend_from_slice(b"\0\0\0\x20stsd\0\0\0\0\0\0\0\x01\0\0\0\x10avc1");
        bytes
    }

    #[test]
    fn a_movie_states_how_long_it_plays_and_how_it_is_encoded() {
        let p = facts("clip.mp4", &movie(65)).unwrap();
        assert_eq!(p.duration_ms, Some(65_000));
        assert_eq!(p.codec.as_deref(), Some("h264"));
        assert_eq!(p.container.as_deref(), Some("mp4"));
        let p = facts("clip.MOV", &movie(2)).unwrap();
        assert_eq!(p.container.as_deref(), Some("quicktime"));
        assert!(facts("clip.mp4", b"not a movie at all").is_none());
    }
}
