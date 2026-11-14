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

/// A picture's format and intrinsic width and height, read from its header: PNG, JPEG, GIF and
/// WebP from their container headers, SVG from the size its root element states or the box its
/// `viewBox` draws in. `None` when `bytes` is no such picture or its header states no size.
#[must_use]
pub fn image_size(bytes: &[u8]) -> Option<(ImageFormat, u32, u32)> {
    if let Some((w, h)) = svg_size(bytes) {
        return Some((SVG, w, h));
    }
    sniff_image(bytes).filter(|&(_, w, h)| w > 0 && h > 0)
}

const SVG: ImageFormat = ImageFormat {
    extension: "svg",
    mime: "image/svg+xml",
};

/// An SVG's width and height: its root element's `width` and `height` when both state a size,
/// else the `viewBox` it draws in (its third and fourth numbers), which is the only size a
/// picture that scales to its container states. `None` when neither does.
fn svg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !is_svg(bytes) {
        return None;
    }
    let tag = svg_root_tag(bytes)?;
    let stated = attribute(tag, "width")
        .and_then(pixel_size)
        .zip(attribute(tag, "height").and_then(pixel_size));
    if let Some((w, h)) = stated {
        return Some((pixels(w), pixels(h)));
    }
    let view_box = attribute(tag, "viewBox")?;
    let numbers: Vec<f64> = view_box
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|number| !number.is_empty())
        .filter_map(|number| number.parse::<f64>().ok())
        .collect();
    let [_, _, w, h] = numbers.as_slice() else {
        return None;
    };
    (w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0).then(|| (pixels(*w), pixels(*h)))
}

/// The `<svg …>` start tag of an SVG document, its attributes as one string.
fn svg_root_tag(bytes: &[u8]) -> Option<&str> {
    let text = bytes.trim_ascii_start();
    let start = text
        .windows(4)
        .position(|window| window == b"<svg")?
        .checked_add(4)?;
    let tag = text.get(start..)?;
    let end = tag.iter().position(|byte| *byte == b'>')?;
    std::str::from_utf8(tag.get(..end)?).ok()
}

/// The value of the attribute `name` in a start tag's attributes.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let starts_a_word = rest
            .get(..at)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|before| !(before.is_alphanumeric() || matches!(before, '-' | '_' | ':')));
        let after_name = rest.get(at.checked_add(name.len())?..)?;
        if starts_a_word
            && let Some(value) = after_name.trim_start().strip_prefix('=')
            && let Some(quote) = value.trim_start().chars().next()
            && matches!(quote, '"' | '\'')
        {
            let quoted = &value.trim_start()[1..];
            return quoted.split(quote).next();
        }
        rest = after_name;
    }
}

/// A `width`/`height` attribute's size in pixels: a number, or a number with `px`. Any other
/// unit (a percentage, a physical unit) states no size a renderer reserves.
fn pixel_size(value: &str) -> Option<f64> {
    let value = value.trim();
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(end);
    let unit = unit.trim();
    if !(unit.is_empty() || unit.eq_ignore_ascii_case("px")) {
        return None;
    }
    let number: f64 = number.parse().ok()?;
    (number.is_finite() && number > 0.0).then_some(number)
}

/// A size in pixels, rounded up so a picture smaller than one pixel still has one.
fn pixels(size: f64) -> u32 {
    let rounded = size.ceil().clamp(1.0, f64::from(u32::MAX));
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to between one and u32::MAX first"
    )]
    let pixels = rounded as u32;
    pixels
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
        Some(SVG)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What reads the size a file states.
    fn size(svg: &str) -> Option<(u32, u32)> {
        image_size(svg.as_bytes()).map(|(_, w, h)| (w, h))
    }

    #[test]
    fn an_svg_states_its_size_in_its_width_and_height() {
        assert_eq!(
            size(r#"<svg width="256" height="128" xmlns="http://www.w3.org/2000/svg"/>"#),
            Some((256, 128))
        );
        assert_eq!(
            size(r#"<?xml version="1.0"?><svg height='20px' width='40px'></svg>"#),
            Some((40, 20))
        );
        // One dimension states none of the picture's size.
        assert_eq!(size(r#"<svg width="256"></svg>"#), None);
        // A size that is a share of the container reserves nothing.
        assert_eq!(
            size(r#"<svg width="100%" height="100%" viewBox="0 0 12 6"/>"#),
            Some((12, 6))
        );
    }

    #[test]
    fn an_svg_without_a_stated_size_takes_it_from_the_view_box() {
        assert_eq!(
            size(r#"<svg viewBox="0 0 64 32" xmlns="http://www.w3.org/2000/svg"/>"#),
            Some((64, 32))
        );
        assert_eq!(size("<svg viewBox='-4,-5, 1.5, 2.25'></svg>"), Some((2, 3)));
        // A box that draws nothing states no size, and neither does no box.
        assert_eq!(size(r#"<svg viewBox="0 0 0 0"></svg>"#), None);
        assert_eq!(size(r#"<svg viewBox="0 0 64"></svg>"#), None);
        assert_eq!(size(r#"<svg></svg>"#), None);
    }

    #[test]
    fn a_page_that_embeds_an_svg_is_not_an_svg() {
        assert_eq!(
            image_size(b"<!doctype html><html><body><svg width=\"2\" height=\"2\"/></body></html>"),
            None
        );
    }
}
