//! Coordination transcripts for inter-device DKG and signing.
//!
//! The DKG and threshold-signing wire types are point-to-point/set
//! messages, normally wrapped in authenticated channels (Iroh
//! direct-message tunnels secured by each device's transport key) that
//! integrity-check them. For the device-pairing ceremony (QR scan +
//! 6-digit code), where no transport channel exists yet, the spec calls
//! for a per-ceremony transcript hash binding: the two devices'
//! transport public keys, the human-confirmable 6-digit code, and the
//! DKG round-1 commitments from both sides.
//!
//! After mutual confirmation the transcript hash anchors the new shared
//! identity; devices that disagree on it abort the ceremony.
//!
//! `PairingTranscript` lives here (not in `shell`/`ui`, which orchestrate
//! the ceremony) so its shape stays fixed in the same crate defining the
//! round-1 message types it references, avoiding drift between what the
//! cryptographer expects and what the UI computes.

use serde::{Deserialize, Serialize};

use collab_types::canonical_bytes;

use crate::dkg::EnrolmentRound1;
use crate::errors::ThresholdError;

/// The bound transcript of a device-pairing ceremony. Built by both
/// devices independently: each broadcasts its own `EnrolmentRound1`
/// commitment and the 6-digit human-confirmed code, then both call
/// [`PairingTranscript::compute_hash`] and check the 32-byte result
/// matches a QR-displayed truncation. A MITM cannot inject a different
/// round-1 commitment without changing the truncated hash, which the
/// user catches by reading it off their other device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingTranscript {
    /// 6-digit human-confirmable code, carried as `u32` (a base-10 6-digit
    /// value fits in 20 bits).
    pub confirmation_code: u32,
    /// Sorted (ascending by `from_share_index`) round-1 commitments — both
    /// devices must agree on the encoding order.
    pub commitments: Vec<EnrolmentRound1>,
}

impl PairingTranscript {
    /// Construct a transcript from the participants' round-1 commitments and
    /// a confirmation code; sorts the commitments deterministically.
    pub fn new(confirmation_code: u32, mut commitments: Vec<EnrolmentRound1>) -> Self {
        commitments.sort_by_key(|c| c.from_share_index);
        Self {
            confirmation_code,
            commitments,
        }
    }

    /// Compute the 32-byte hash via postcard + SHA-512 truncation (reusing
    /// SHA-512 for simplicity — the FROST cipher suite already pulls it in).
    /// UIs typically display the first 6 hex chars as a "fingerprint" the
    /// human cross-confirms with their other device.
    ///
    /// # Errors
    ///
    /// Returns an error if `self` cannot be canonically encoded
    /// (effectively unreachable).
    pub fn compute_hash(&self) -> Result<[u8; 32], ThresholdError> {
        // Hash via sha2 (already a transitive dep).
        use sha2::Digest;

        // Canonicalise via postcard for byte-level determinism across machines.
        let bytes = canonical_bytes(self)?;
        let mut hasher = sha2::Sha512::new();
        hasher.update(b"tilo-threshold-identity:pairing-transcript:v1");
        hasher.update(bytes);
        let full = hasher.finalize();
        // SHA-512 output is always 64 bytes, so this never actually misses —
        // `.get()` avoids trusting that rather than indexing blindly.
        let head = full.get(..32).ok_or_else(|| {
            ThresholdError::FrostInternal("SHA-512 output shorter than 32 bytes".to_string())
        })?;
        let mut out = [0u8; 32];
        out.copy_from_slice(head);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcript hash is independent of commitment order but binds the
    /// commitment bytes and the confirmation code.
    #[test]
    fn transcript_hash_binds_commitments_and_code_but_not_their_order() {
        // Case: order independence — same commitments in different order
        // produce the same transcript hash; the constructor sorts.
        let a = EnrolmentRound1 {
            from_share_index: 1,
            package_bytes: vec![1, 2, 3],
        };
        let b = EnrolmentRound1 {
            from_share_index: 2,
            package_bytes: vec![4, 5, 6],
        };
        let t1 = PairingTranscript::new(123_456, vec![a.clone(), b.clone()]);
        let t2 = PairingTranscript::new(123_456, vec![b, a]);
        assert_eq!(t1.compute_hash().unwrap(), t2.compute_hash().unwrap());

        // Case: the hash changes with commitment bytes.
        let a = EnrolmentRound1 {
            from_share_index: 1,
            package_bytes: vec![1, 2, 3],
        };
        let a2 = EnrolmentRound1 {
            from_share_index: 1,
            package_bytes: vec![1, 2, 4],
        };
        let b = EnrolmentRound1 {
            from_share_index: 2,
            package_bytes: vec![5, 6, 7],
        };
        let t1 = PairingTranscript::new(1, vec![a, b.clone()]);
        let t2 = PairingTranscript::new(1, vec![a2, b]);
        assert_ne!(t1.compute_hash().unwrap(), t2.compute_hash().unwrap());

        // Case: the hash changes with the code.
        let a = EnrolmentRound1 {
            from_share_index: 1,
            package_bytes: vec![1, 2, 3],
        };
        let t1 = PairingTranscript::new(111_111, vec![a.clone()]);
        let t2 = PairingTranscript::new(222_222, vec![a]);
        assert_ne!(t1.compute_hash().unwrap(), t2.compute_hash().unwrap());
    }
}
