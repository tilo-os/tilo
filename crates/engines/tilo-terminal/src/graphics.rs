//! Terminal image placements, protocol neutral, and a bounded Kitty APC scanner.

use base64::Engine as _;

const MAX_APC_BYTES: usize = 12 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
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
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub protocol_id: Option<u32>,
    pub alternate_screen: bool,
}

#[derive(Debug, PartialEq, Eq)]
/// A complete Kitty graphics transmission, already parsed and bounded.
pub struct KittyImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub columns: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub protocol_id: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Bytes(Vec<u8>),
    Kitty(Vec<u8>),
}

#[derive(Default)]
pub struct GraphicsScanner {
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

pub fn parse_kitty(payload: &[u8]) -> Option<KittyImage> {
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
        columns: columns.clamp(1, MAX_PLACEMENT_CELLS),
        rows: rows.clamp(1, MAX_PLACEMENT_CELLS),
        pixel_width,
        pixel_height,
        protocol_id,
    })
}

/// Whether a placement's bytes are the picture its metadata names: a replicated placement can
/// carry malformed metadata, so a view validates again before it decodes anything.
#[must_use]
pub fn placement_is_sound(placement: &TerminalImagePlacement, bytes: &[u8]) -> bool {
    if bytes.len() > MAX_IMAGE_BYTES
        || !dimensions_allowed(placement.pixel_width, placement.pixel_height)
    {
        return false;
    }
    match placement.mime.as_str() {
        "image/png" => {
            png_dimensions(bytes) == Some((placement.pixel_width, placement.pixel_height))
        }
        "application/x-kitty-rgba" => expected_bytes(placement) == u64::try_from(bytes.len()).ok(),
        "application/x-kitty-rgb" => expected_bytes(placement)
            .and_then(|bytes| bytes.checked_mul(3))
            .is_some_and(|rgb| rgb / 4 == u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
        _ => false,
    }
}

fn expected_bytes(placement: &TerminalImagePlacement) -> Option<u64> {
    u64::from(placement.pixel_width)
        .checked_mul(u64::from(placement.pixel_height))?
        .checked_mul(4)
}

fn dimensions_allowed(width: u32, height: u32) -> bool {
    width != 0
        && height != 0
        && u64::from(width)
            .checked_mul(u64::from(height))
            .is_some_and(|pixels| pixels <= MAX_DECODED_PIXELS)
}

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
