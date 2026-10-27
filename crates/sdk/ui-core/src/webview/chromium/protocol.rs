use std::io::{self, Read, Write};
use std::sync::Arc;

use wire_frame::Framing;

use crate::webview::Frame;

pub(super) const MAX_PACKET: u32 = 64 * 1024 * 1024;

/// A little-endian `u32` length counting the kind byte and body, then the
/// kind byte, then the body.
const FRAMING: Framing = Framing::little_endian(MAX_PACKET as usize).reject_empty();

pub(super) enum Packet {
    Json(serde_json::Value),
    Frame(WireFrame),
    TabFrame(u32, WireFrame),
    /// What changed of a tab's frame (`super::damage`).
    TabDamage(u32, super::damage::WireDamage),
    /// A frame the host put into the shared frame memory (`frame_memory`).
    SharedFrame(SharedFrame),
}

/// The host's notice that a tab's frame waits in a slot of the frame memory.
/// Its dimensions are validated as a piped frame's are; its slot is the
/// memory's to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SharedFrame {
    pub(super) tab: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) slot: u32,
}

impl SharedFrame {
    /// The frame's size in bytes, valid by construction.
    pub(super) fn size(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// The frame notice's kind byte.
const SHARED_FRAME: u8 = 5;

/// A frame as the host sends it: BGRA, as it came off the wire. The owner of a
/// shared host passes a client tab's frames on exactly so; only the process
/// that draws one converts it, in place (`into_frame`).
pub(super) struct WireFrame {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bgra: Vec<u8>,
}

impl WireFrame {
    /// The frame to draw: its pixels turned to RGBA in their own buffer.
    pub(super) fn into_frame(self, tab: u32) -> Frame {
        let Self {
            width,
            height,
            bgra: mut pixels,
        } = self;
        bgra_to_rgba(&mut pixels);
        rgba_frame(tab, width, height, pixels)
    }
}

/// The frame to draw from pixels already in RGBA.
pub(super) fn rgba_frame(tab: u32, width: u32, height: u32, rgba: Vec<u8>) -> Frame {
    Frame {
        trace_id: crate::webview::frame_trace::received(tab, &rgba, width, height),
        damage: None,
        width,
        height,
        rgba: Arc::new(rgba),
    }
}

/// A frame's size in bytes, when its dimensions are ones the host may send.
fn frame_size(width: u32, height: u32) -> Option<u32> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|size| *size < MAX_PACKET)
}

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Swaps each pixel's blue and red in place, two pixels to a 64-bit word:
/// swapping the bytes of one pixel at a time took 2.4 times as long.
pub(super) fn bgra_to_rgba(pixels: &mut [u8]) {
    const BLUE_AND_RED: u64 = 0x0000_00FF_0000_00FF;
    let mut pairs = pixels.chunks_exact_mut(8);
    for pair in &mut pairs {
        let mut word = [0; 8];
        word.copy_from_slice(pair);
        let word = u64::from_le_bytes(word);
        let word = (word & !(BLUE_AND_RED | (BLUE_AND_RED << 16)))
            | ((word >> 16) & BLUE_AND_RED)
            | ((word & BLUE_AND_RED) << 16);
        pair.copy_from_slice(&word.to_le_bytes());
    }
    for pixel in pairs.into_remainder().chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

/// `bgra`'s pixels in RGBA, converted as they are copied: one pass over the
/// pixels, where a copy and then an in-place swap took two.
pub(super) fn rgba_from_bgra(bgra: &[u8]) -> Vec<u8> {
    const BLUE_AND_RED: u64 = 0x0000_00FF_0000_00FF;
    // Zeroed by the allocator's fresh pages, not by a pass of its own.
    let mut rgba = vec![0; bgra.len()];
    let mut from = bgra.chunks_exact(8);
    let mut to = rgba.chunks_exact_mut(8);
    for (source, target) in (&mut from).zip(&mut to) {
        let mut word = [0; 8];
        word.copy_from_slice(source);
        let word = u64::from_le_bytes(word);
        let word = (word & !(BLUE_AND_RED | (BLUE_AND_RED << 16)))
            | ((word >> 16) & BLUE_AND_RED)
            | ((word & BLUE_AND_RED) << 16);
        target.copy_from_slice(&word.to_le_bytes());
    }
    for (source, target) in from
        .remainder()
        .chunks_exact(4)
        .zip(to.into_remainder().chunks_exact_mut(4))
    {
        target.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
    }
    rgba
}

pub(super) fn write_json(writer: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    let prefix = bytes
        .len()
        .checked_add(1)
        .and_then(|len| FRAMING.encode_prefix(len).ok())
        .ok_or_else(|| invalid("Chromium command exceeds packet limit"))?;
    writer.write_all(&prefix)?;
    writer.write_all(&[1])?;
    writer.write_all(&bytes)?;
    writer.flush()
}

/// Writes `frame` as the host would: a routed frame packet carrying its BGRA
/// pixels as they are.
pub(super) fn write_tab_frame(
    writer: &mut impl Write,
    tab: u32,
    frame: &WireFrame,
) -> io::Result<()> {
    let size = frame
        .width
        .checked_mul(frame.height)
        .and_then(|pixels| pixels.checked_mul(4));
    if size.and_then(|size| usize::try_from(size).ok()) != Some(frame.bgra.len()) {
        return Err(invalid("Frame size does not match its dimensions"));
    }
    let prefix = frame
        .bgra
        .len()
        .checked_add(13)
        .and_then(|len| FRAMING.encode_prefix(len).ok())
        .ok_or_else(|| invalid("Chromium frame exceeds packet limit"))?;
    writer.write_all(&prefix)?;
    writer.write_all(&[3])?;
    writer.write_all(&tab.to_le_bytes())?;
    writer.write_all(&frame.width.to_le_bytes())?;
    writer.write_all(&frame.height.to_le_bytes())?;
    writer.write_all(&frame.bgra)?;
    writer.flush()
}

/// Writes `frame`'s notice as the host would.
#[cfg(test)]
pub(super) fn write_shared_frame(writer: &mut impl Write, frame: &SharedFrame) -> io::Result<()> {
    writer.write_all(&17u32.to_le_bytes())?;
    writer.write_all(&[SHARED_FRAME])?;
    for field in [frame.tab, frame.width, frame.height, frame.slot] {
        writer.write_all(&field.to_le_bytes())?;
    }
    writer.flush()
}

pub(super) fn read_packet(reader: &mut impl Read) -> io::Result<Option<Packet>> {
    let Some(length) = FRAMING.read_len(reader)? else {
        return Ok(None);
    };
    let length = u32::try_from(length).map_err(|_| invalid("Invalid Chromium packet length"))?;
    let mut kind = [0];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        1 => {
            let payload_size = length
                .checked_sub(1)
                .ok_or_else(|| invalid("Invalid Chromium packet length"))?;
            let mut bytes = zeroize::Zeroizing::new(vec![0; payload_size as usize]);
            reader.read_exact(&mut bytes)?;
            Ok(Some(Packet::Json(serde_json::from_slice(&bytes)?)))
        }
        SHARED_FRAME => {
            if length != 17 {
                return Err(invalid("Invalid Chromium frame notice"));
            }
            let mut fields = [0; 16];
            reader.read_exact(&mut fields)?;
            let field = |at: usize| {
                u32::from_le_bytes([fields[at], fields[at + 1], fields[at + 2], fields[at + 3]])
            };
            let frame = SharedFrame {
                tab: field(0),
                width: field(4),
                height: field(8),
                slot: field(12),
            };
            if frame.tab == 0 || frame.tab > i32::MAX as u32 {
                return Err(invalid("Invalid tab ID"));
            }
            if frame_size(frame.width, frame.height).is_none() {
                return Err(invalid("Invalid Chromium frame dimensions"));
            }
            Ok(Some(Packet::SharedFrame(frame)))
        }
        2 | 3 => {
            let header_size = if kind[0] == 3 { 13 } else { 9 };
            let payload_size = length
                .checked_sub(header_size)
                .ok_or_else(|| invalid("Truncated Chromium frame header"))?;
            let tab = if kind[0] == 3 {
                let mut bytes = [0; 4];
                reader.read_exact(&mut bytes)?;
                let id = u32::from_le_bytes(bytes);
                if id == 0 || id > i32::MAX as u32 {
                    return Err(invalid("Invalid tab ID"));
                }
                Some(id)
            } else {
                None
            };
            let mut dimensions = [0; 8];
            reader.read_exact(&mut dimensions)?;
            let width =
                u32::from_le_bytes(dimensions[..4].try_into().map_err(|_| invalid("width"))?);
            let height =
                u32::from_le_bytes(dimensions[4..].try_into().map_err(|_| invalid("height"))?);
            if frame_size(width, height) != Some(payload_size) {
                return Err(invalid("Invalid Chromium frame dimensions"));
            }
            let mut bgra = vec![0; payload_size as usize];
            reader.read_exact(&mut bgra)?;
            let frame = WireFrame {
                width,
                height,
                bgra,
            };
            Ok(Some(match tab {
                Some(id) => Packet::TabFrame(id, frame),
                None => Packet::Frame(frame),
            }))
        }
        4 => {
            let (tab, damage) = super::damage::read_body(reader, length)?;
            Ok(Some(Packet::TabDamage(tab, damage)))
        }
        _ => Err(invalid("Unknown Chromium packet kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn packets_and_frames_are_pinned_bounded_and_validated() {
        // phase: json_packet_round_trip_and_eof
        {
            let mut bytes = Vec::new();
            write_json(
                &mut bytes,
                &serde_json::json!({"op":"navigate","url":"https://tilo.so"}),
            )
            .unwrap();
            let mut reader = Cursor::new(bytes);
            let Some(Packet::Json(value)) = read_packet(&mut reader).unwrap() else {
                panic!("expected JSON")
            };
            assert_eq!(value["url"], "https://tilo.so");
            assert!(read_packet(&mut reader).unwrap().is_none());
        }
        // phase: packet_wire_bytes_are_pinned
        // The helper's packets: a little-endian `u32` length that counts the
        // kind byte, the kind, then the body.
        {
            let mut bytes = Vec::new();
            write_json(&mut bytes, &serde_json::json!({"a": 1})).unwrap();
            assert_eq!(bytes, b"\x08\0\0\0\x01{\"a\":1}");
            let Some(Packet::Json(value)) = read_packet(&mut Cursor::new(&bytes)).unwrap() else {
                panic!("expected JSON")
            };
            assert_eq!(value["a"], 1);
        }
        // phase: malformed_packets_are_rejected_before_allocation
        {
            for bytes in [
                vec![1, 0],
                vec![0, 0, 0, 0],
                (MAX_PACKET + 1).to_le_bytes().to_vec(),
                vec![2, 0, 0, 0, 1],
                vec![1, 0, 0, 0, 9],
            ] {
                assert!(read_packet(&mut Cursor::new(bytes)).is_err());
            }
        }
        // phase: bgra_frame_is_validated_and_converted
        {
            let mut bytes = 13u32.to_le_bytes().to_vec();
            bytes.push(2);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&[10, 20, 30, 255]);
            let Some(Packet::Frame(frame)) = read_packet(&mut Cursor::new(&bytes)).unwrap() else {
                panic!("expected frame")
            };
            assert_eq!(frame.bgra, [10, 20, 30, 255], "read as sent");
            let frame = frame.into_frame(1);
            assert_eq!((frame.width, frame.height), (1, 1));
            assert_eq!(&*frame.rgba, &[30, 20, 10, 255]);
            bytes[5..9].copy_from_slice(&8193u32.to_le_bytes());
            assert!(read_packet(&mut Cursor::new(bytes)).is_err());
        }
        // phase: frame_notice_is_pinned_and_validated
        {
            let notice = SharedFrame {
                tab: 7,
                width: 3,
                height: 2,
                slot: 1,
            };
            let mut bytes = Vec::new();
            write_shared_frame(&mut bytes, &notice).unwrap();
            assert_eq!(
                bytes,
                b"\x11\0\0\0\x05\x07\0\0\0\x03\0\0\0\x02\0\0\0\x01\0\0\0"
            );
            let Some(Packet::SharedFrame(read)) = read_packet(&mut Cursor::new(&bytes)).unwrap()
            else {
                panic!("expected a frame notice")
            };
            assert_eq!((read, read.size()), (notice, 24));
            for bad in [
                SharedFrame { tab: 0, ..notice },
                SharedFrame {
                    tab: 1 << 31,
                    ..notice
                },
                SharedFrame { width: 0, ..notice },
                SharedFrame {
                    height: 8193,
                    ..notice
                },
                SharedFrame {
                    width: 8192,
                    height: 8192,
                    ..notice
                },
            ] {
                let mut bytes = Vec::new();
                write_shared_frame(&mut bytes, &bad).unwrap();
                assert!(read_packet(&mut Cursor::new(bytes)).is_err(), "{bad:?}");
            }
            let mut long = bytes.clone();
            long[0] = 18;
            long.push(0);
            assert!(read_packet(&mut Cursor::new(long)).is_err());
        }
        // phase: a_copied_frame_is_converted_like_one_swapped_in_place
        {
            let bgra: Vec<u8> = (0..44).collect();
            let mut swapped = bgra.clone();
            bgra_to_rgba(&mut swapped);
            assert_eq!(rgba_from_bgra(&bgra), swapped);
            assert_eq!(rgba_from_bgra(&bgra[..12]), swapped[..12]);
        }
        // phase: every_pixel_of_an_odd_width_frame_is_converted
        {
            let frame = WireFrame {
                width: 3,
                height: 1,
                bgra: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            };
            assert_eq!(
                &*frame.into_frame(1).rgba,
                &[3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12]
            );
        }
    }
}
