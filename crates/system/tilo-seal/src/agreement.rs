//! Static-static key agreement: one symmetric key two devices share without
//! ever sending a message.
//!
//! - `dh = X25519(own secret, peer public)`; an all-zero result (a
//!   low-order peer key) is refused;
//! - key: `HKDF-SHA256(salt = none, ikm = dh, info = label || lo || hi)`,
//!   where `lo` and `hi` are the two X25519 public keys in byte order, so
//!   both sides derive the same key and it is bound to exactly this pair.
//!
//! The key is the same every time for the same two key pairs. Callers that
//! put anything derived from it on the wire mix in fresh randomness, so what
//! an observer sees does not repeat.

use hpke_rs_crypto::HpkeCrypto as _;
use hpke_rs_crypto::types::KemAlgorithm;
use hpke_rs_rust_crypto::HpkeRustCrypto;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{KEY_LEN, PublicKey, SecretKey, SymmetricKey};
use crate::label::Label;

/// The key `own` and `peer` share for `label`.
///
/// # Errors
///
/// [`Error::InvalidKey`] when `peer` is a low-order point or the primitive
/// rejects a key.
pub fn shared_key(own: &SecretKey, peer: &PublicKey, label: Label) -> Result<SymmetricKey> {
    let own_public = own.public_key()?;
    let dh = Zeroizing::new(
        HpkeRustCrypto::dh(
            KemAlgorithm::DhKem25519,
            peer.as_bytes(),
            own.expose_secret(),
        )
        .map_err(|_| Error::InvalidKey)?,
    );
    let (lo, hi) = if own_public.as_bytes() <= peer.as_bytes() {
        (own_public, *peer)
    } else {
        (*peer, own_public)
    };
    let mut info = label.as_bytes().to_vec();
    info.extend_from_slice(lo.as_bytes());
    info.extend_from_slice(hi.as_bytes());
    let mut out = Zeroizing::new([0_u8; KEY_LEN]);
    hkdf::Hkdf::<sha2::Sha256>::new(None, dh.as_ref())
        .expand(&info, out.as_mut())
        .map_err(|_| Error::InvalidKey)?;
    Ok(SymmetricKey::from_bytes(*out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_derive_one_key_bound_to_the_pair_and_label() {
        let alice = SecretKey::generate().unwrap();
        let bob = SecretKey::generate().unwrap();
        let carol = SecretKey::generate().unwrap();
        let ab = shared_key(&alice, &bob.public_key().unwrap(), Label::LanDiscovery).unwrap();
        let ba = shared_key(&bob, &alice.public_key().unwrap(), Label::LanDiscovery).unwrap();
        assert_eq!(ab, ba);
        let ac = shared_key(&alice, &carol.public_key().unwrap(), Label::LanDiscovery).unwrap();
        assert_ne!(ab, ac);
        let other_purpose = shared_key(
            &alice,
            &bob.public_key().unwrap(),
            Label::DeviceFabricSecret,
        )
        .unwrap();
        assert_ne!(ab, other_purpose);
    }

    #[test]
    fn low_order_peer_keys_are_refused() {
        let alice = SecretKey::generate().unwrap();
        for point in [[0_u8; 32], {
            let mut one = [0_u8; 32];
            one[0] = 1;
            one
        }] {
            assert_eq!(
                shared_key(&alice, &PublicKey::from_bytes(point), Label::LanDiscovery),
                Err(Error::InvalidKey)
            );
        }
    }
}
