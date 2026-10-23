//! Member-device compaction of a sealed document route.
//!
//! The host cannot read a sealed route, so it cannot tell which retained
//! entries a snapshot contains. It asks a member device instead
//! ([`crate::Envelope::CompactRequest`]). The member, which opened every
//! entry it was delivered, publishes a sealed snapshot of its replica and
//! then a [`CoverageStatement`] signed with its device key: the entries the
//! snapshot contains, named by [`entry_id`], a digest of the retained body
//! both sides hold byte for byte. The host removes exactly those entries and
//! keeps everything else, including every entry the member never saw.

use serde::{Deserialize, Serialize};

const STATEMENT_DOMAIN: &[u8] = b"tilo-compaction-statement-v1\0";

/// Identity of one retained document entry: a digest of the
/// `NeighbourMsg::AppDocument` body the host retains and delivers.
pub fn entry_id(body: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-retained-entry-v1\0");
    hash.update(body);
    *hash.finalize().as_bytes()
}

/// What a member's snapshot of one route contains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageStatement {
    pub workspace_id: u64,
    /// Retention channel of the route.
    pub channel: [u8; 32],
    /// The host request this answers.
    pub request: [u8; 16],
    /// Entry ids of the snapshot's own frames, which the member committed
    /// before this statement.
    pub snapshot: Vec<[u8; 32]>,
    /// Entry ids whose every change the snapshot contains.
    pub covered: Vec<[u8; 32]>,
    /// Automerge heads of the snapshot, for devices that audit it.
    pub heads: Vec<[u8; 32]>,
}

impl CoverageStatement {
    /// Canonical encoding, as signed.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Decode a statement; trailing bytes are refused.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        let (statement, rest) = postcard::take_from_bytes(bytes)?;
        if rest.is_empty() {
            Ok(statement)
        } else {
            Err(postcard::Error::DeserializeBadEncoding)
        }
    }
}

/// The bytes a device signs for an encoded statement.
pub fn signing_bytes(statement: &[u8]) -> Vec<u8> {
    [STATEMENT_DOMAIN, statement].concat()
}

/// Whether `signature` is `device`'s signature over the encoded `statement`.
pub fn verify(device: &[u8; 32], statement: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(device) else {
        return false;
    };
    key.verify_strict(
        &signing_bytes(statement),
        &ed25519_dalek::Signature::from_bytes(signature),
    )
    .is_ok()
}

/// The heads an Automerge snapshot records in its header, read without
/// loading the document.
pub fn snapshot_heads(bytes: &[u8]) -> Option<Vec<[u8; 32]>> {
    fn uleb(bytes: &[u8], at: &mut usize) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *bytes.get(*at)?;
            *at = at.checked_add(1)?;
            value |= u64::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift = shift.checked_add(7).filter(|shift| *shift < 64)?;
        }
    }
    const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
    if bytes.get(..4)? != MAGIC || *bytes.get(8)? != 0 {
        return None;
    }
    let mut at = 9;
    uleb(bytes, &mut at)?;
    for _ in 0..uleb(bytes, &mut at)? {
        let len = usize::try_from(uleb(bytes, &mut at)?).ok()?;
        at = at.checked_add(len)?;
    }
    let count = usize::try_from(uleb(bytes, &mut at)?).ok()?;
    let mut heads = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        heads.push(bytes.get(at..at.checked_add(32)?)?.try_into().ok()?);
        at = at.checked_add(32)?;
    }
    Some(heads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;

    #[test]
    fn a_statement_verifies_only_for_its_device_and_bytes() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let device = key.verifying_key().to_bytes();
        let statement = CoverageStatement {
            workspace_id: 7,
            channel: [1; 32],
            request: [2; 16],
            snapshot: vec![[4; 32]],
            covered: vec![entry_id(b"a"), entry_id(b"b")],
            heads: vec![[5; 32]],
        }
        .encode()
        .unwrap();
        let signature = key.sign(&signing_bytes(&statement)).to_bytes();
        assert!(verify(&device, &statement, &signature));
        assert!(!verify(&[9; 32], &statement, &signature));
        let mut tampered = statement.clone();
        tampered[0] ^= 1;
        assert!(!verify(&device, &tampered, &signature));
        assert!(CoverageStatement::decode(&[statement.as_slice(), &[0]].concat()).is_err());
    }

    #[test]
    fn entry_ids_bind_the_whole_body() {
        assert_ne!(entry_id(b"body"), entry_id(b"body "));
    }
}
