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
}

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
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Frame {
            trace_id: crate::webview::frame_trace::received(tab, &pixels, width, height),
            width,
            height,
            rgba: Arc::new(pixels),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
            let size = width.checked_mul(height).and_then(|n| n.checked_mul(4));
            if width == 0
                || height == 0
                || width > 8192
                || height > 8192
                || size != Some(payload_size)
            {
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
    }
}
