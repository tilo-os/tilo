//! Protocol-neutral terminal image placements and a bounded Kitty APC scanner.
//!
//! Placements name bytes by the same BLAKE3 address used by Tilo documents/files,
//! so collaborators exchange a tiny immutable descriptor and resolve the blob
//! locally.  This module accepts a deliberately small, safe Kitty subset:
//! complete direct `a=T,t=d` PNG, RGB and RGBA uploads.  File/shared-memory
//! transports, chunking and mutable image ids are refused rather than giving a
//! terminal process access to paths, shared memory, or unbounded assembly.

use base64::Engine as _;
use std::sync::Arc;
use ui::widget::Image;

const MAX_APC_BYTES: usize = 12 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Keep PNG decompression from turning a small compressed blob into a huge GPU
/// allocation.  This is also used for raw pixels, whose decoded form is exact.
const MAX_DECODED_PIXELS: u64 = 8 * 1024 * 1024;
const MAX_PLACEMENT_CELLS: u32 = 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalImagePlacement {
    pub hash: String,
    pub mime: String,
    pub byte_len: u64,
    pub row: usize,
    pub col: usize,
    pub columns: u32,
    pub rows: u32,
    /// Intrinsic source width and height.  PNG carries these in IHDR; Kitty raw
    /// RGB(A) supplies `s`/`v`.  Persisting them lets another replica decode and
    /// paint a raw placement without host-local protocol state.
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub protocol_id: Option<u32>,
    pub alternate_screen: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KittyImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub columns: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub protocol_id: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamEvent {
    Bytes(Vec<u8>),
    Kitty(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct GraphicsScanner {
    state: ScanState,
    ordinary: Vec<u8>,
}

#[derive(Default)]
enum ScanState {
    #[default]
    Ground,
    Escape,
    ApcStart,
    GenericApc,
    GenericApcEscape,
    Kitty(Vec<u8>),
    KittyEscape(Vec<u8>),
    OversizeKitty,
    OversizeKittyEscape,
}

impl GraphicsScanner {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for &byte in bytes {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                ScanState::Ground if byte == 0x1b => ScanState::Escape,
                ScanState::Ground => {
                    self.ordinary.push(byte);
                    ScanState::Ground
                }
                ScanState::Escape if byte == b'_' => {
                    Self::flush_bytes(&mut self.ordinary, &mut events);
                    ScanState::ApcStart
                }
                ScanState::Escape => {
                    self.ordinary.extend_from_slice(&[0x1b, byte]);
                    ScanState::Ground
                }
                ScanState::ApcStart if byte == b'G' => ScanState::Kitty(Vec::new()),
                ScanState::GenericApc if byte == 0x1b => ScanState::GenericApcEscape,
                ScanState::GenericApcEscape | ScanState::OversizeKittyEscape if byte == b'\\' => {
                    ScanState::Ground
                }
                ScanState::ApcStart | ScanState::GenericApc | ScanState::GenericApcEscape => {
                    ScanState::GenericApc
                }
                ScanState::Kitty(payload) if byte == 0x1b => ScanState::KittyEscape(payload),
                ScanState::Kitty(mut payload) => {
                    if payload.len() >= MAX_APC_BYTES {
                        ScanState::OversizeKitty
                    } else {
                        payload.push(byte);
                        ScanState::Kitty(payload)
                    }
                }
                ScanState::KittyEscape(payload) if byte == b'\\' => {
                    events.push(StreamEvent::Kitty(payload));
                    ScanState::Ground
                }
                ScanState::KittyEscape(mut payload) => {
                    if payload.len().saturating_add(2) > MAX_APC_BYTES {
                        ScanState::OversizeKitty
                    } else {
                        payload.extend_from_slice(&[0x1b, byte]);
                        ScanState::Kitty(payload)
                    }
                }
                ScanState::OversizeKitty if byte == 0x1b => ScanState::OversizeKittyEscape,
                ScanState::OversizeKitty | ScanState::OversizeKittyEscape => {
                    ScanState::OversizeKitty
                }
            };
        }
        Self::flush_bytes(&mut self.ordinary, &mut events);
        events
    }

    fn flush_bytes(bytes: &mut Vec<u8>, events: &mut Vec<StreamEvent>) {
        if !bytes.is_empty() {
            events.push(StreamEvent::Bytes(std::mem::take(bytes)));
        }
    }
}

pub(crate) fn parse_kitty(payload: &[u8]) -> Option<KittyImage> {
    let separator = payload.iter().position(|byte| *byte == b';')?;
    let (control, encoded_with_separator) = payload.split_at(separator);
    let encoded = encoded_with_separator.get(1..)?;
    let mut action = b't';
    let mut format = 100u32;
    let mut transmission = b'd';
    let mut more = false;
    let mut columns = 0;
    let mut rows = 0;
    let mut pixel_width = None;
    let mut pixel_height = None;
    let mut protocol_id = None;
    for part in control.split(|byte| *byte == b',') {
        let Some(separator) = part.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value_with_separator) = part.split_at(separator);
        let Some(value) = value_with_separator.get(1..) else {
            continue;
        };
        match key {
            b"a" => action = *value.first()?,
            b"f" => format = std::str::from_utf8(value).ok()?.parse().ok()?,
            b"t" => transmission = *value.first()?,
            b"m" => more = value == b"1",
            b"c" => columns = std::str::from_utf8(value).ok()?.parse().ok()?,
            b"r" => rows = std::str::from_utf8(value).ok()?.parse().ok()?,
            b"s" => pixel_width = Some(std::str::from_utf8(value).ok()?.parse().ok()?),
            b"v" => pixel_height = Some(std::str::from_utf8(value).ok()?.parse().ok()?),
            b"i" => protocol_id = std::str::from_utf8(value).ok()?.parse().ok(),
            _ => {}
        }
    }
    // First foundation: one direct, complete transmit-and-place command. File,
    // shared-memory and chunk continuations require ownership/lifetime rules
    // outside the terminal process and are rejected rather than guessed.
    if action != b'T' || transmission != b'd' || more {
        return None;
    }
    let mime = match format {
        100 => "image/png",
        32 => "application/x-kitty-rgba",
        24 => "application/x-kitty-rgb",
        _ => return None,
    };
    let estimated = encoded.len().saturating_mul(3) / 4;
    if estimated > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let (pixel_width, pixel_height) = match format {
        100 => png_dimensions(&bytes)?,
        24 | 32 => {
            let (width, height) = (pixel_width?, pixel_height?);
            let channels = if format == 24 { 3u64 } else { 4u64 };
            let expected = u64::from(width)
                .checked_mul(u64::from(height))?
                .checked_mul(channels)?;
            if expected != u64::try_from(bytes.len()).ok()? || !dimensions_allowed(width, height) {
                return None;
            }
            (width, height)
        }
        _ => return None,
    };
    Some(KittyImage {
        bytes,
        mime,
        // Zero means "use natural size" in the full Kitty protocol.  There is
        // no terminal-independent natural cell size, so our subset makes it a
        // single cell; dimensions are still retained for a sharp paint.
        columns: columns.clamp(1, MAX_PLACEMENT_CELLS),
        rows: rows.clamp(1, MAX_PLACEMENT_CELLS),
        pixel_width,
        pixel_height,
        protocol_id,
    })
}

/// Decode a replicated placement only after applying the same bounds the
/// stream parser uses.  A remote CRDT replica can contain malformed metadata,
/// so rendering must validate again instead of trusting the original host.
pub(crate) fn decode_for_rendering(
    placement: &TerminalImagePlacement,
    bytes: &[u8],
) -> Option<Image> {
    if bytes.len() > MAX_IMAGE_BYTES
        || !dimensions_allowed(placement.pixel_width, placement.pixel_height)
    {
        return None;
    }
    match placement.mime.as_str() {
        "image/png" => {
            let (width, height) = png_dimensions(bytes)?;
            (width == placement.pixel_width && height == placement.pixel_height)
                .then(|| Image::from_encoded(bytes).ok())?
        }
        "application/x-kitty-rgb" | "application/x-kitty-rgba" => {
            let channels = if placement.mime.ends_with("rgba") {
                4usize
            } else {
                3usize
            };
            let pixels = usize::try_from(
                u64::from(placement.pixel_width) * u64::from(placement.pixel_height),
            )
            .ok()?;
            if bytes.len() != pixels.checked_mul(channels)? {
                return None;
            }
            let rgba = if channels == 4 {
                bytes.to_vec()
            } else {
                let mut rgba = Vec::with_capacity(pixels.checked_mul(4)?);
                for rgb in bytes.chunks_exact(3) {
                    let Ok([red, green, blue]) = <&[u8; 3]>::try_from(rgb) else {
                        return None;
                    };
                    rgba.extend_from_slice(&[*red, *green, *blue, 255]);
                }
                rgba
            };
            Some(Image::from_rgba(
                Arc::from(rgba),
                placement.pixel_width,
                placement.pixel_height,
            ))
        }
        _ => None,
    }
}

fn dimensions_allowed(width: u32, height: u32) -> bool {
    width != 0
        && height != 0
        && u64::from(width)
            .checked_mul(u64::from(height))
            .is_some_and(|pixels| pixels <= MAX_DECODED_PIXELS)
}

/// Read and validate a PNG IHDR without decompressing it.  `Image` performs
/// the actual decode afterwards, but this gate prevents an image bomb from
/// reaching its allocation path.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    let header = bytes.get(..24)?;
    if header.get(..8)? != PNG_SIGNATURE
        || header.get(8..12)? != [0, 0, 0, 13]
        || header.get(12..16)? != b"IHDR"
    {
        return None;
    }
    let width = u32::from_be_bytes(header.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(header.get(20..24)?.try_into().ok()?);
    dimensions_allowed(width, height).then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kitty APC scanning preserves ANSI, and parsing accepts only bounded, complete, exactly-sized images.
    #[test]
    fn kitty_graphics_scanning_and_parsing_is_bounded_and_exact() {
        // Case `scanner_preserves_ansi_and_extracts_split_kitty_apc`.
        {
            let mut scanner = GraphicsScanner::default();
            assert_eq!(
                scanner.push(b"hello\x1b"),
                vec![StreamEvent::Bytes(b"hello".to_vec())]
            );
            assert!(scanner.push(b"_Gf=100,c=2,r=3,i=7;AAAA\x1b").is_empty());
            assert_eq!(
                scanner.push(b"\\world"),
                vec![
                    StreamEvent::Kitty(b"f=100,c=2,r=3,i=7;AAAA".to_vec()),
                    StreamEvent::Bytes(b"world".to_vec())
                ]
            );
        }

        // Case `parser_accepts_only_bounded_direct_complete_images`.
        {
            let image = parse_kitty(b"a=T,f=100,t=d,c=4,r=2,i=9;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
            assert!(!image.bytes.is_empty());
            assert_eq!(
                (
                    image.columns,
                    image.rows,
                    image.pixel_width,
                    image.pixel_height,
                    image.protocol_id
                ),
                (4, 2, 1, 1, Some(9))
            );
            assert!(parse_kitty(b"a=T,f=100,t=f;L3RtcC9pbWFnZQ==").is_none());
            assert!(parse_kitty(b"a=T,f=100,m=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").is_none());
        }

        // Case `raw_rgb_requires_declared_exact_dimensions`.
        {
            let image = parse_kitty(b"a=T,f=24,t=d,s=2,v=1,c=2,r=1;AQIDBAUG").unwrap();
            assert_eq!(image.bytes, [1, 2, 3, 4, 5, 6]);
            assert_eq!((image.pixel_width, image.pixel_height), (2, 1));
            assert!(parse_kitty(b"a=T,f=24,t=d,s=2,v=1;AQID").is_none());
        }
    }

    /// The rendering decoder revalidates raw metadata and decodes validated PNGs.
    #[test]
    fn rendering_decoder_revalidates_metadata_and_decodes_png() {
        // Case `rendering_decoder_revalidates_raw_metadata`.
        {
            let placement = TerminalImagePlacement {
                hash: "test".into(),
                mime: "application/x-kitty-rgb".into(),
                byte_len: 6,
                row: 0,
                col: 0,
                columns: 1,
                rows: 1,
                pixel_width: 2,
                pixel_height: 1,
                protocol_id: None,
                alternate_screen: false,
            };
            let decoded = decode_for_rendering(&placement, &[1, 2, 3, 4, 5, 6]).unwrap();
            assert_eq!(decoded.intrinsic_size(), (2, 1));
            assert!(decode_for_rendering(&placement, &[1, 2, 3]).is_none());
        }

        // Case `rendering_decoder_decodes_validated_png`.
        {
            let image = parse_kitty(b"a=T,f=100,t=d;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
            let placement = TerminalImagePlacement {
                hash: "test".into(),
                mime: image.mime.into(),
                byte_len: image.bytes.len() as u64,
                row: 0,
                col: 0,
                columns: image.columns,
                rows: image.rows,
                pixel_width: image.pixel_width,
                pixel_height: image.pixel_height,
                protocol_id: None,
                alternate_screen: false,
            };
            assert_eq!(
                decode_for_rendering(&placement, &image.bytes)
                    .unwrap()
                    .intrinsic_size(),
                (1, 1)
            );
        }
    }
}
