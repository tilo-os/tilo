//! Fast, bounded extraction of facts safe to replicate as file metadata.
//!
//! This module only sees bytes already present for a local write/preview. It
//! never opens a path or asks the hydrator for remote content.

use fs_protocol::{FilePropertiesV1, files::FILE_PROPERTIES_VERSION};

pub const MAX_METADATA_SCAN_BYTES: usize = 16 * 1024 * 1024;

/// Extensions [`extract`] reads facts from; keep in step with its match.
const SCANNED_EXTENSIONS: &[&str] = &["png", "gif", "jpg", "jpeg", "wav", "pdf"];

/// Whether [`extract`] can find anything in a file called `name`, so a
/// caller can skip reading bytes for it.
pub fn recognizes(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .filter(|ext| *ext != name)
        .is_some_and(|ext| {
            SCANNED_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "header offsets are fixed and guarded by format-specific length checks"
)]
pub fn extract(name: &str, bytes: &[u8]) -> Option<FilePropertiesV1> {
    let bytes = &bytes[..bytes.len().min(MAX_METADATA_SCAN_BYTES)];
    let ext = name
        .rsplit('.')
        .next()
        .filter(|ext| *ext != name)?
        .to_ascii_lowercase();
    let mut value = FilePropertiesV1 {
        schema_version: FILE_PROPERTIES_VERSION,
        image_width: None,
        image_height: None,
        duration_ms: None,
        page_count: None,
        codec: None,
        container: None,
    };
    match ext.as_str() {
        "png" | "gif" | "jpg" | "jpeg" => {
            let (format, width, height) = tilo_content::image_size(bytes)?;
            // Metadata names JPEG `jpeg`, whichever spelling the file uses.
            let name = if format.extension == "jpg" {
                "jpeg"
            } else {
                format.extension
            };
            let named = if ext == "jpg" { "jpeg" } else { ext.as_str() };
            if name != named {
                return None;
            }
            value.image_width = Some(width);
            value.image_height = Some(height);
            value.codec = Some(name.into());
            value.container = Some(name.into());
        }
        "wav" => {
            let (duration_ms, codec) = wav_properties(bytes)?;
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
        _ => return None,
    }
    value.validate().ok().map(|()| value)
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::indexing_slicing,
    reason = "RIFF chunk bounds are checked before fixed-width header reads"
)]
fn wav_properties(bytes: &[u8]) -> Option<(u64, &'static str)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_metadata_extracts_only_from_matching_content() {
        // Phase `extracts_png_dimensions`.
        {
            let mut png = vec![0; 24];
            png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
            png[16..20].copy_from_slice(&640u32.to_be_bytes());
            png[20..24].copy_from_slice(&480u32.to_be_bytes());
            let p = extract("photo.png", &png).unwrap();
            assert_eq!((p.image_width, p.image_height), (Some(640), Some(480)));
        }
        // Phase `extracts_pdf_page_count_without_unbounded_input`.
        {
            let p = extract("a.pdf", b"%PDF-1.7 /Type /Pages /Type /Page /Type /Page\n").unwrap();
            assert_eq!(p.page_count, Some(2));
        }
        // Phase `extracts_wav_duration_and_codec`.
        {
            let mut wav = Vec::from(&b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x40\x1f\x00\x00\x01\x00\x08\x00data\x40\x1f\x00\x00"[..]);
            wav.resize(wav.len() + 8_000, 0);
            let p = extract("tone.wav", &wav).unwrap();
            assert_eq!(p.duration_ms, Some(1_000));
            assert_eq!(p.codec.as_deref(), Some("pcm"));
            assert_eq!(p.container.as_deref(), Some("wav"));
        }
        // Phase `rejects_spoofed_extension`.
        {
            assert!(extract("photo.png", b"not a png").is_none());
        }
    }
}
