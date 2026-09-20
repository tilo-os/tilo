//! Bounded encoding of committed state on the low-latency desktop lane.
pub const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;

use std::io::Write;
pub fn encode(packets: &[Vec<u8>]) -> Result<Vec<String>, String> {
    if packets.is_empty()
        || packets.len() > 64
        || packets.iter().map(Vec::len).sum::<usize>() > MAX_DECODED_BYTES
    {
        return Err("committed desktop exceeds decoded byte budget".into());
    }
    packets
        .iter()
        .map(|packet| {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(packet).map_err(|e| e.to_string())?;
            let compressed = encoder.finish().map_err(|e| e.to_string())?;
            Ok(if compressed.len() + 1 < packet.len() {
                format!("z:{}", hex::encode(compressed))
            } else {
                hex::encode(packet)
            })
        })
        .collect()
}
pub fn decode(packets: &[String]) -> Result<Vec<Vec<u8>>, String> {
    if packets.is_empty()
        || packets.len() > 64
        || packets.iter().map(String::len).sum::<usize>() > 256 * 1024
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
        assert!(super::encode(&[vec![0; super::MAX_DECODED_BYTES + 1]]).is_err());
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
}
