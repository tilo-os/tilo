//! Protocol-neutral terminal image placements and a bounded Kitty APC scanner.
//!
//! Rendering is intentionally not part of this module. A placement names bytes
//! by the same BLAKE3 address used by qOS documents/files, so collaboration can
//! replicate the descriptor cheaply and fetch the immutable payload through the
//! shared blob transport. The renderer can be added without changing the wire or
//! CRDT model.

use base64::Engine as _;

const MAX_APC_BYTES: usize = 12 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalImagePlacement {
    pub hash: String,
    pub mime: String,
    pub byte_len: u64,
    pub row: usize,
    pub col: usize,
    pub columns: u32,
    pub rows: u32,
    pub protocol_id: Option<u32>,
    pub alternate_screen: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KittyImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub columns: u32,
    pub rows: u32,
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
                ScanState::ApcStart => ScanState::GenericApc,
                ScanState::GenericApc if byte == 0x1b => ScanState::GenericApcEscape,
                ScanState::GenericApc => ScanState::GenericApc,
                ScanState::GenericApcEscape if byte == b'\\' => ScanState::Ground,
                ScanState::GenericApcEscape => ScanState::GenericApc,
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
                ScanState::OversizeKitty => ScanState::OversizeKitty,
                ScanState::OversizeKittyEscape if byte == b'\\' => ScanState::Ground,
                ScanState::OversizeKittyEscape => ScanState::OversizeKitty,
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
            b"i" => protocol_id = std::str::from_utf8(value).ok()?.parse().ok(),
            _ => {}
        }
    }
    // First foundation: one direct, complete transmit-and-place command. File,
    // shared-memory and chunk continuations require ownership/lifetime rules
    // outside the terminal process and are rejected rather than guessed.
    if !matches!(action, b't' | b'T') || transmission != b'd' || more {
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
    (bytes.len() <= MAX_IMAGE_BYTES).then_some(KittyImage {
        bytes,
        mime,
        columns,
        rows,
        protocol_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_preserves_ansi_and_extracts_split_kitty_apc() {
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

    #[test]
    fn parser_accepts_only_bounded_direct_complete_images() {
        let image = parse_kitty(b"a=T,f=100,t=d,c=4,r=2,i=9;aGVsbG8=").unwrap();
        assert_eq!(image.bytes, b"hello");
        assert_eq!(
            (image.columns, image.rows, image.protocol_id),
            (4, 2, Some(9))
        );
        assert!(parse_kitty(b"a=T,f=100,t=f;L3RtcC9pbWFnZQ==").is_none());
        assert!(parse_kitty(b"a=T,f=100,m=1;aGVsbG8=").is_none());
    }
}
