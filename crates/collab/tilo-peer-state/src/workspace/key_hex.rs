//! Fixed-size lowercase hex text of 32-byte keys, without allocation.

/// The 64 lowercase hex digits of a 32-byte key or digest.
#[derive(Clone, Copy)]
pub(crate) struct KeyHex([u8; 64]);

impl KeyHex {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        let mut text = [0; 64];
        for (pair, byte) in text.chunks_exact_mut(2).zip(key) {
            pair.copy_from_slice(&[digit(byte >> 4), digit(byte & 0x0f)]);
        }
        Self(text)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }

    pub(crate) fn as_str(&self) -> &str {
        // Every byte is an ASCII hex digit, so the fallback is never taken.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

fn digit(nibble: u8) -> u8 {
    // `nibble` is below 16, so neither step saturates.
    if nibble < 10 {
        b'0'.saturating_add(nibble)
    } else {
        b'a'.saturating_add(nibble.saturating_sub(10))
    }
}

#[cfg(test)]
mod tests {
    use super::KeyHex;

    #[test]
    fn key_hex_matches_the_hex_crate() {
        let key: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap().wrapping_mul(37));
        assert_eq!(KeyHex::new(&key).as_str(), hex::encode(key));
    }
}
