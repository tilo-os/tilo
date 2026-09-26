//! Bounded cache codec, shared by production and the focused test harness.

pub(super) const LIMIT: usize = 32 * 1024 * 1024;
const ENTRY_LIMIT: usize = 1024 * 1024;
const MAGIC: &[u8] = b"TILODXC1";

pub(super) struct Entry {
    pub(super) key: Vec<u8>,
    pub(super) shader: Vec<u8>,
}

pub(super) fn encode(identity: &[u8; 32], entries: &[Entry]) -> Option<Vec<u8>> {
    if entries.len() > 256 {
        return None;
    }
    let mut body = Vec::new();
    body.extend_from_slice(&u32::try_from(entries.len()).ok()?.to_le_bytes());
    for entry in entries {
        for part in [&entry.key, &entry.shader] {
            if part.is_empty() || part.len() > ENTRY_LIMIT {
                return None;
            }
            body.extend_from_slice(&u32::try_from(part.len()).ok()?.to_le_bytes());
            body.extend_from_slice(part);
            if body.len() > LIMIT.saturating_sub(72) {
                return None;
            }
        }
    }
    let mut output = Vec::with_capacity(72 + body.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(identity);
    output.extend_from_slice(blake3::hash(&body).as_bytes());
    output.extend_from_slice(&body);
    Some(output)
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    let part = bytes.get(..count)?;
    *bytes = bytes.get(count..)?;
    Some(part)
}

fn number(bytes: &mut &[u8]) -> Option<usize> {
    usize::try_from(u32::from_le_bytes(take(bytes, 4)?.try_into().ok()?)).ok()
}

pub(super) fn decode(mut bytes: &[u8], identity: &[u8; 32]) -> Option<Vec<Entry>> {
    if bytes.len() > LIMIT || take(&mut bytes, 8)? != MAGIC || take(&mut bytes, 32)? != identity {
        return None;
    }
    let checksum = take(&mut bytes, 32)?;
    if checksum != blake3::hash(bytes).as_bytes() {
        return None;
    }
    let count = number(&mut bytes)?;
    if count > 256 {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    let mut keys = std::collections::HashSet::new();
    for _ in 0..count {
        let key_len = number(&mut bytes)?;
        if key_len == 0 || key_len > ENTRY_LIMIT {
            return None;
        }
        let key = take(&mut bytes, key_len)?;
        if !keys.insert(key) {
            return None;
        }
        let shader_len = number(&mut bytes)?;
        if shader_len == 0 || shader_len > ENTRY_LIMIT {
            return None;
        }
        entries.push(Entry {
            key: key.to_vec(),
            shader: take(&mut bytes, shader_len)?.to_vec(),
        });
    }
    bytes.is_empty().then_some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<Entry> {
        vec![Entry {
            key: b"actual-HLSL-and-flags".to_vec(),
            shader: b"compiled-DXIL".to_vec(),
        }]
    }
    #[test]
    fn shader_cache_codec_round_trips_and_rejects_every_corruption() {
        // Case `persisted_shader_round_trip_and_compiler_invalidation`.
        {
            let bytes = encode(&[1; 32], &sample()).unwrap();
            let entries = decode(&bytes, &[1; 32]).unwrap();
            assert_eq!(entries[0].key, sample()[0].key);
            assert_eq!(entries[0].shader, sample()[0].shader);
            assert!(decode(&bytes, &[2; 32]).is_none());
        }

        // Case `rejects_truncation_bit_corruption_and_trailing_data`.
        {
            let bytes = encode(&[1; 32], &sample()).unwrap();
            for length in 0..bytes.len() {
                assert!(decode(&bytes[..length], &[1; 32]).is_none());
            }
            for index in 0..bytes.len() {
                let mut corrupt = bytes.clone();
                corrupt[index] ^= 1;
                assert!(decode(&corrupt, &[1; 32]).is_none());
            }
            let mut extended = bytes;
            extended.push(0);
            assert!(decode(&extended, &[1; 32]).is_none());
        }

        // Case `rejects_duplicate_keys_empty_shaders_and_oversized_entries`.
        {
            let mut duplicate = sample();
            duplicate.extend(sample());
            assert!(decode(&encode(&[1; 32], &duplicate).unwrap(), &[1; 32]).is_none());
            let mut entry = sample();
            entry[0].shader.clear();
            assert!(encode(&[1; 32], &entry).is_none());
            entry[0].shader.resize(ENTRY_LIMIT + 1, 0);
            assert!(encode(&[1; 32], &entry).is_none());
        }

        // Case `rejects_valid_checksum_over_invalid_structure`.
        {
            let mut bytes = encode(&[1; 32], &sample()).unwrap();
            bytes[72..76].copy_from_slice(&u32::MAX.to_le_bytes());
            let hash = blake3::hash(&bytes[72..]);
            bytes[40..72].copy_from_slice(hash.as_bytes());
            assert!(decode(&bytes, &[1; 32]).is_none());
        }
    }
}
