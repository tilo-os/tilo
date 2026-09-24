//! RNG bridge into frost-ed25519.
//!
//! This crate's API takes the workspace RNG traits (`rand_core` 0.10, shared
//! with ed25519-dalek 3). frost-ed25519 3.0 still consumes `rand_core` 0.6,
//! which it re-exports; [`FrostRng`] forwards every call unchanged so a
//! caller's RNG drives FROST exactly as it drives the single-device path.

use frost_ed25519::rand_core as frost_rand_core;

/// Borrow a `rand_core` 0.10 CSPRNG as the `rand_core` 0.6 RNG frost expects.
pub(crate) struct FrostRng<'a, R: ?Sized>(pub(crate) &'a mut R);

impl<R: rand_core::CryptoRng + ?Sized> frost_rand_core::RngCore for FrostRng<'_, R> {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), frost_rand_core::Error> {
        self.0.fill_bytes(dest);
        Ok(())
    }
}

impl<R: rand_core::CryptoRng + ?Sized> frost_rand_core::CryptoRng for FrostRng<'_, R> {}

#[cfg(test)]
mod tests {
    use super::*;
    use frost_rand_core::RngCore as _;
    use rand_core::{Rng as _, SeedableRng as _};

    #[test]
    fn forwards_the_wrapped_stream_unchanged() {
        let mut direct = rand_chacha::ChaCha20Rng::from_seed([9; 32]);
        let mut wrapped = rand_chacha::ChaCha20Rng::from_seed([9; 32]);
        let mut bridge = FrostRng(&mut wrapped);
        assert_eq!(bridge.next_u32(), direct.next_u32());
        assert_eq!(bridge.next_u64(), direct.next_u64());
        let (mut a, mut b) = ([0_u8; 37], [0_u8; 37]);
        bridge.fill_bytes(&mut a);
        direct.fill_bytes(&mut b);
        assert_eq!(a, b);
        bridge.try_fill_bytes(&mut a).unwrap();
        direct.fill_bytes(&mut b);
        assert_eq!(a, b);
    }
}
