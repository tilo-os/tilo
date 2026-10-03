//! `quickXorHash`, the content hash every `OneDrive` flavour reports.
//!
//! A 160-bit value: each input byte is folded (XOR) into a 160-bit ring at a bit
//! position advancing by 11 per byte, and the total length is folded into
//! the last 64 bits, little endian. Reported base64 encoded.

use base64::Engine as _;

const WIDTH_BITS: usize = 160;
const SHIFT: usize = 11;
/// After this many bytes the bit position repeats (11 × 160 bits).
const PERIOD: usize = SHIFT * WIDTH_BITS;
const SIZE: usize = WIDTH_BITS / 8;

/// An incremental `quickXorHash`.
#[derive(Clone)]
pub struct QuickXorHash {
    /// Input bytes folded by their position modulo [`PERIOD`].
    data: Box<[u8; PERIOD]>,
    len: u64,
}

impl Default for QuickXorHash {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickXorHash {
    pub fn new() -> Self {
        Self {
            data: Box::new([0; PERIOD]),
            len: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        // `len % PERIOD` is below PERIOD, so it fits usize on every target.
        let mut position =
            usize::try_from(self.len.checked_rem(PERIOD as u64).unwrap_or(0)).unwrap_or(0);
        for byte in bytes {
            if let Some(slot) = self.data.get_mut(position) {
                *slot ^= byte;
            }
            position = position.wrapping_add(1);
            if position == PERIOD {
                position = 0;
            }
        }
        self.len = self.len.wrapping_add(bytes.len() as u64);
    }

    /// The 20 raw hash bytes.
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "bit positions are reduced modulo 160 and indices stay below 21"
    )]
    pub fn digest(&self) -> [u8; SIZE] {
        let mut out = [0_u8; SIZE + 1];
        for (index, byte) in self.data.iter().enumerate() {
            let shift = (index * SHIFT) % WIDTH_BITS;
            let (whole, bits) = (shift / 8, shift % 8);
            let shifted = u16::from(*byte) << bits;
            out[whole] ^= shifted.to_le_bytes()[0];
            out[whole + 1] ^= shifted.to_le_bytes()[1];
        }
        out[0] ^= out[SIZE];
        for (index, byte) in self.len.to_le_bytes().iter().enumerate() {
            out[SIZE - 8 + index] ^= byte;
        }
        let mut digest = [0_u8; SIZE];
        digest.copy_from_slice(&out[..SIZE]);
        digest
    }

    /// The hash as Graph reports it.
    pub fn base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.digest())
    }
}

/// `quickXorHash` of `bytes`, base64 encoded.
pub fn quick_xor_base64(bytes: &[u8]) -> String {
    let mut hash = QuickXorHash::new();
    hash.update(bytes);
    hash.base64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(text: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap()
    }

    /// Vectors from rclone's reference implementation
    /// (`backend/onedrive/quickxorhash`), input and output base64.
    #[test]
    fn matches_reference_vectors() {
        for (input, expected) in [
            ("", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ("Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
            ("0pZP", "0rDEEwAAAAAAAAAAAwAAAAAAAAA="),
            ("h490d57Pqz5q2rtT", "h3gEHe7giWeswgdq3MYupgAAAAA="),
            ("2fuxhBJXtpWFe8dOfdGeHw==", "O9tHLAghgSvYohKFyMMxnNCHaHg="),
            (
                "NbYXsp5/K6mR+NmHwExjvWeWDJFnXTKWVlzYHoesp2E=",
                "wjuAuWDiq04qDt1R8hHWDDcwVoQ=",
            ),
        ] {
            assert_eq!(quick_xor_base64(&decode(input)), expected, "{input}");
        }
    }

    #[test]
    fn incremental_updates_match_one_shot_across_the_period() {
        let bytes: Vec<u8> = (0..5000_u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut hash = QuickXorHash::new();
        for chunk in bytes.chunks(333) {
            hash.update(chunk);
        }
        assert_eq!(hash.base64(), quick_xor_base64(&bytes));
    }
}
