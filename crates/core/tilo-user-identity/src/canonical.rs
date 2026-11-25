//! Canonical byte-encoding of everything that gets signed.
//!
//! All signed types in this crate — `DeviceCert`, `RevocationEntry`,
//! and every workspace-directory delta — encode
//! their signing preimage through the same three-step pipeline:
//!
//! 1. Build a view of the struct that omits its `signature` field.
//! 2. Encode the view with postcard directly into the hasher.
//! 3. Prepend a fixed byte-constant "domain tag" (`tilo-devcert-v1\0`
//!    etc.) and hash the concatenation with BLAKE3-256.
//!
//! The Ed25519 signature is then produced over the 32-byte hash, not
//! the raw bytes. Two reasons the design pins this ordering:
//!
//! - **Domain separation**: the tag stops a signature made under
//!   one tag from being replayed under another (a `DeviceCert`
//!   signature can never be reinterpreted as a `RevocationEntry`
//!   signature, no matter how similar the byte layout).
//! - **Wire-ABI stability**: the encoded view is what is signed, so a
//!   signature over one layout never verifies over another.
//!
//! This module holds the postcard helpers + the BLAKE3 wrapper.
//! The domain-tag constants themselves live next to the types they
//! sign (`cert::DEVCERT_PREIMAGE_TAG`, etc.) so a reader looking at
//! a type sees its tag in the same file.

pub use collab_types::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
pub use wire_frame::canonical::signing_digest;

#[cfg(test)]
mod streaming_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Fixture {
        a: u32,
        b: String,
    }

    tilo_test_env::scenario! {
        fn canonical_bytes_round_trip_exactly_and_digests_are_domain_separated;

        phase round_trip_via_canonical_bytes;
        {
            let f = Fixture {
                a: 42,
                b: "hi".into(),
            };
            let bytes = canonical_bytes(&f).unwrap();
            let back: Fixture = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(f, back);
        }

        phase rejects_trailing_bytes;
        {
            let f = Fixture {
                a: 1,
                b: "x".into(),
            };
            let mut bytes = canonical_bytes(&f).unwrap();
            bytes.push(0xff);
            assert!(from_canonical_bytes::<Fixture>(&bytes).is_err());
        }

        phase domain_tag_changes_digest;
        // Different domain tags produce different digests even for
        // the same body — pinning the domain-separation property.
        {
            let f = Fixture {
                a: 1,
                b: "same".into(),
            };
            let d1 = signing_digest(b"tag-a\0", &f).unwrap();
            let d2 = signing_digest(b"tag-b\0", &f).unwrap();
            assert_ne!(d1, d2);
        }

        phase digest_is_deterministic;
        // Digest is deterministic across independent runs.
        {
            let f = Fixture {
                a: 1,
                b: "same".into(),
            };
            assert_eq!(
                signing_digest(b"tag-a\0", &f).unwrap(),
                signing_digest(b"tag-a\0", &f).unwrap(),
            );
        }
    }
}
