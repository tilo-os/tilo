//! Bounded encoding of committed state on the low-latency desktop lane.
pub const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_WIRE_BYTES: usize = 256 * 1024;

use std::io::Write;

pub struct EncodedPacket {
    encoded: String,
    decoded_bytes: usize,
}

impl EncodedPacket {
    pub fn wire_bytes(&self) -> usize {
        self.encoded.len()
    }
}

pub fn encode_packet(packet: &[u8]) -> Result<EncodedPacket, String> {
    if packet.len() > MAX_DECODED_BYTES {
        return Err("committed desktop exceeds decoded byte budget".into());
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(packet).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    let encoded = if compressed.len() + 1 < packet.len() {
        format!("z:{}", hex::encode(compressed))
    } else {
        hex::encode(packet)
    };
    Ok(EncodedPacket {
        encoded,
        decoded_bytes: packet.len(),
    })
}

pub struct Encoder {
    packets: Vec<String>,
    decoded_bytes: usize,
    wire_bytes: usize,
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            packets: Vec::new(),
            decoded_bytes: 0,
            wire_bytes: 0,
        }
    }

    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    pub fn preflight_raw(&self, packet: &[u8]) -> Result<(), String> {
        if self.packets.len() >= 64
            || self
                .decoded_bytes
                .checked_add(packet.len())
                .is_none_or(|bytes| bytes > MAX_DECODED_BYTES)
        {
            return Err("committed desktop exceeds decoded byte budget".into());
        }
        Ok(())
    }

    pub fn push(&mut self, packet: EncodedPacket) -> Result<(), String> {
        if self.packets.len() >= 64
            || self
                .decoded_bytes
                .checked_add(packet.decoded_bytes)
                .is_none_or(|bytes| bytes > MAX_DECODED_BYTES)
            || self
                .wire_bytes
                .checked_add(packet.wire_bytes())
                .is_none_or(|bytes| bytes > MAX_WIRE_BYTES)
        {
            return Err("committed desktop exceeds transport budget".into());
        }
        self.decoded_bytes += packet.decoded_bytes;
        self.wire_bytes += packet.wire_bytes();
        self.packets.push(packet.encoded);
        Ok(())
    }

    pub fn into_packets(self) -> Result<Vec<String>, String> {
        (!self.packets.is_empty())
            .then_some(self.packets)
            .ok_or_else(|| "committed desktop has no packets".into())
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn encode(packets: &[Vec<u8>]) -> Result<Vec<String>, String> {
    let mut encoder = Encoder::new();
    for packet in packets {
        encoder.preflight_raw(packet)?;
        encoder.push(encode_packet(packet)?)?;
    }
    encoder.into_packets()
}
pub fn decode(packets: &[String]) -> Result<Vec<Vec<u8>>, String> {
    if packets.is_empty()
        || packets.len() > 64
        || packets.iter().map(String::len).sum::<usize>() > MAX_WIRE_BYTES
    {
        return Err("committed desktop exceeds wire budget".into());
    }
    let mut remaining = MAX_DECODED_BYTES;
    packets
        .iter()
        .map(|packet| {
            let bytes = if let Some(compressed) = packet.strip_prefix("z:") {
                let encoded = hex::decode(compressed).map_err(|e| e.to_string())?;
                let mut decoder = flate2::Decompress::new(true);
                let mut decoded = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let before_in = decoder.total_in();
                    let before_out = decoder.total_out();
                    let capacity = chunk.len().min(remaining.saturating_sub(decoded.len()) + 1);
                    let status = decoder
                        .decompress(
                            &encoded[before_in as usize..],
                            &mut chunk[..capacity],
                            flate2::FlushDecompress::None,
                        )
                        .map_err(|e| e.to_string())?;
                    let written = (decoder.total_out() - before_out) as usize;
                    if decoded.len() + written > remaining {
                        return Err("committed desktop expansion exceeds budget".into());
                    }
                    decoded.extend_from_slice(&chunk[..written]);
                    if status == flate2::Status::StreamEnd {
                        if decoder.total_in() != encoded.len() as u64 {
                            return Err("trailing compressed desktop bytes".into());
                        }
                        break;
                    }
                    if decoder.total_in() == before_in && written == 0 {
                        return Err("incomplete compressed desktop packet".into());
                    }
                }
                decoded
            } else {
                hex::decode(packet).map_err(|e| e.to_string())?
            };
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or("committed desktop expansion exceeds budget")?;
            Ok(bytes)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn catchup_history_fits_private_lane_without_losing_bytes() {
        let bytes = (0..80)
            .flat_map(|index| {
                let mut row = vec![42u8; 5000];
                row[..8].copy_from_slice(&(index as u64).to_le_bytes());
                row
            })
            .collect::<Vec<_>>();
        let encoded = super::encode(&[bytes.clone()]).unwrap();
        assert!(serde_json::to_vec(&encoded).unwrap().len() < 256 * 1024);
        assert_eq!(super::decode(&encoded).unwrap(), vec![bytes]);
    }
    #[test]
    fn rejects_truncated_checksums() {
        let encoded = super::encode(&[vec![42; 10000]]).unwrap();
        for missing in [1, 4] {
            let truncated = encoded[0][..encoded[0].len() - missing * 2].to_owned();
            assert!(super::decode(&[truncated]).is_err());
        }
    }
    #[test]
    fn refuses_oversized_expansion_and_malformed_packets() {
        assert!(super::decode(&["z:not hex".into()]).is_err());
        assert!(super::decode(&["z:0001".into()]).is_err());
        let too_large = vec![0; super::MAX_DECODED_BYTES + 1];
        assert!(super::encode_packet(&too_large).is_err());
        assert!(super::encode(&[too_large]).is_err());
        use std::io::Write;
        let mut bomb = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        bomb.write_all(&vec![0; super::MAX_DECODED_BYTES + 1])
            .unwrap();
        assert!(super::decode(&[format!("z:{}", hex::encode(bomb.finish().unwrap()))]).is_err());
        assert_eq!(
            super::decode(&["010203".into()]).unwrap(),
            vec![vec![1, 2, 3]]
        );
    }

    #[test]
    fn encoder_refuses_a_packet_the_decoder_would_reject_for_wire_size() {
        let mut state = 0x9e37_79b9u32;
        let bytes = (0..(super::MAX_WIRE_BYTES / 2 + 4096))
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect::<Vec<_>>();
        let packet = super::encode_packet(&bytes).unwrap();
        assert!(packet.wire_bytes() > super::MAX_WIRE_BYTES);
        let encoded = packet.encoded.clone();
        assert!(super::Encoder::new().push(packet).is_err());
        assert!(super::decode(&[encoded]).is_err());
    }
}
