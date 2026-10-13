//! What a piece of content is, read from its first bytes: media type and, for images, the
//! intrinsic size a renderer reserves before the pixels arrive.

use crate::ContentMeta;

/// Derive a descriptor from content: media type plus intrinsic dimensions, read from the container
/// header rather than a full decode (the point is only to know how much space to reserve). An
/// unrecognised format still gets a correct `len` and reserves no particular size.
pub fn sniff(bytes: &[u8]) -> ContentMeta {
    let mut m = ContentMeta {
        len: bytes.len() as u64,
        ..Default::default()
    };
    if let Some((format, w, h)) = sniff_image(bytes) {
        m.mime = format.mime.to_string();
        m.w = w;
        m.h = h;
    }
    m
}

/// A picture format, named by its file extension and media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageFormat {
    pub extension: &'static str,
    pub mime: &'static str,
}

const PNG: ImageFormat = ImageFormat {
    extension: "png",
    mime: "image/png",
};
const JPEG: ImageFormat = ImageFormat {
    extension: "jpg",
    mime: "image/jpeg",
};
const GIF: ImageFormat = ImageFormat {
    extension: "gif",
    mime: "image/gif",
};
const WEBP: ImageFormat = ImageFormat {
    extension: "webp",
    mime: "image/webp",
};

/// A PNG, JPEG, GIF or WebP picture's format and intrinsic width and height, read from its
/// header. `None` when `bytes` is no such picture or its header states no size.
#[must_use]
pub fn image_size(bytes: &[u8]) -> Option<(ImageFormat, u32, u32)> {
    sniff_image(bytes).filter(|&(_, w, h)| w > 0 && h > 0)
}

/// The picture format `bytes` start with, from the signature alone: PNG, JPEG, GIF, WebP or SVG.
/// Unlike [`sniff`] it needs no complete header, so it also answers for a truncated prefix.
#[must_use]
pub fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(PNG)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(JPEG)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(GIF)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(WEBP)
    } else if is_svg(bytes) {
        Some(ImageFormat {
            extension: "svg",
            mime: "image/svg+xml",
        })
    } else {
        None
    }
}

/// An SVG document, possibly behind leading whitespace and an XML declaration.
fn is_svg(bytes: &[u8]) -> bool {
    let text = bytes.trim_ascii_start();
    text.starts_with(b"<svg")
        || (text.starts_with(b"<?xml")
            && text
                .get(..text.len().min(1024))
                .is_some_and(|head| head.windows(4).any(|window| window == b"<svg")))
}

/// Reads a big-endian `u32` at byte offset `at`, or `None` if `b` is too short.
fn read_u32be(b: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(b.get(at..end)?.try_into().ok()?))
}

/// Reads a little-endian `u16` at byte offset `at`, or `None` if `b` is too short.
fn read_u16le(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(b.get(at..end)?.try_into().ok()?))
}

/// Reads a big-endian `u16` at byte offset `at`, or `None` if `b` is too short.
fn read_u16be(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_be_bytes(b.get(at..end)?.try_into().ok()?))
}

fn sniff_image(b: &[u8]) -> Option<(ImageFormat, u32, u32)> {
    // PNG: 8-byte signature, then an IHDR chunk whose width/height are the two
    // big-endian u32s at offset 16.
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some((PNG, read_u32be(b, 16)?, read_u32be(b, 20)?));
    }
    // GIF: logical screen descriptor at offset 6, little-endian u16s.
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        let w = u32::from(read_u16le(b, 6)?);
        let h = u32::from(read_u16le(b, 8)?);
        return Some((GIF, w, h));
    }
    // WebP: a RIFF container whose lossy/lossless/extended chunks each state the
    // canvas size in a different place.
    if b.len() >= 30 && b.starts_with(b"RIFF") && b.get(8..12).is_some_and(|s| s == b"WEBP") {
        match b.get(12..16) {
            Some(b"VP8X") => {
                let (b24, b25, b26) = (
                    u32::from(*b.get(24)?),
                    u32::from(*b.get(25)?),
                    u32::from(*b.get(26)?),
                );
                let (b27, b28, b29) = (
                    u32::from(*b.get(27)?),
                    u32::from(*b.get(28)?),
                    u32::from(*b.get(29)?),
                );
                let w = (b24 | (b25 << 8) | (b26 << 16)).saturating_add(1);
                let h = (b27 | (b28 << 8) | (b29 << 16)).saturating_add(1);
                return Some((WEBP, w, h));
            }
            Some(b"VP8 ") => {
                let w = u32::from(read_u16le(b, 26)? & 0x3FFF);
                let h = u32::from(read_u16le(b, 28)? & 0x3FFF);
                return Some((WEBP, w, h));
            }
            Some(b"VP8L") => {
                let end = 21usize.checked_add(4)?;
                let bits = u32::from_le_bytes(b.get(21..end)?.try_into().ok()?);
                return Some((
                    WEBP,
                    (bits & 0x3FFF).saturating_add(1),
                    ((bits >> 14) & 0x3FFF).saturating_add(1),
                ));
            }
            _ => return Some((WEBP, 0, 0)),
        }
    }
    // JPEG: walk the marker segments to the start-of-frame, honouring each segment's declared length
    // to skip the entropy-coded scan (which a scan for the next 0xFF would not).
    if b.len() >= 4 && b.first() == Some(&0xFF) && b.get(1) == Some(&0xD8) {
        let mut i = 2usize;
        while i.checked_add(3).is_some_and(|end| end < b.len()) {
            let Some(&byte_at_i) = b.get(i) else {
                break;
            };
            if byte_at_i != 0xFF {
                i = i.saturating_add(1);
                continue;
            }
            let Some(&marker) = b.get(i.saturating_add(1)) else {
                break;
            };
            // Standalone markers carry no payload.
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i = i.saturating_add(2);
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break; // end of image, or the start of the scan data
            }
            if i.saturating_add(3) >= b.len() {
                break;
            }
            let Some(len) = read_u16be(b, i.saturating_add(2)).map(usize::from) else {
                break;
            };
            let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
            if is_sof && i.saturating_add(9) < b.len() {
                let h = u32::from(read_u16be(b, i.saturating_add(5))?);
                let w = u32::from(read_u16be(b, i.saturating_add(7))?);
                return Some((JPEG, w, h));
            }
            if len < 2 {
                break;
            }
            i = i.saturating_add(2).saturating_add(len);
        }
        return Some((JPEG, 0, 0));
    }
    None
}
