//! Signed observed metadata/history coupling, never an immutable-byte archive.
//! Verification needs the actual prepared receipt and privately enrolled key;
//! decoding alone proves neither source custody nor durable completion.
use super::{LIMIT, PreparedReceipt, VerifierClaims, id, refused};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::io;
const DOMAIN: &[u8] = b"tilo-retained-external-replacement-commit-v1\0";
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedReceipt {
    pub schema: u32,
    pub operation_id: String,
    pub source_owner: [u8; 32],
    pub intent_digest: [u8; 32],
    pub personal_room: String,
    pub person: [u8; 32],
    pub device: [u8; 32],
    pub transaction_digest: [u8; 32],
    pub archive_binding_digest: [u8; 32],
    pub source_identity: [u64; 2],
    pub victim_identity: [u64; 2],
    pub destination_identity: [u64; 2],
    pub capture_id: String,
    pub journal_commit_digest: [u8; 32],
    pub source_record_id: String,
    pub source_version_id: String,
    pub victim_record_id: String,
    pub victim_version_id: String,
    pub signature: Vec<u8>,
}
impl CommittedReceipt {
    fn preparation(&self) -> PreparedReceipt {
        PreparedReceipt {
            schema: self.schema,
            operation_id: self.operation_id.clone(),
            source_owner: self.source_owner,
            intent_digest: self.intent_digest,
            personal_room: self.personal_room.clone(),
            person: self.person,
            device: self.device,
            transaction_digest: self.transaction_digest,
            archive_binding_digest: self.archive_binding_digest,
            signature: Vec::new(),
        }
    }
    pub fn signing_bytes(&self) -> io::Result<Vec<u8>> {
        self.preparation().signing_bytes()?;
        if !id(&self.capture_id)
            || !id(&self.source_record_id)
            || !id(&self.source_version_id)
            || !id(&self.victim_record_id)
            || !id(&self.victim_version_id)
            || self.source_record_id == self.victim_record_id
            || self.source_version_id == self.victim_version_id
            || self.journal_commit_digest == [0; 32]
            || self.source_identity[1] == 0
            || self.victim_identity[1] == 0
            || self.source_identity == self.victim_identity
            || self.destination_identity != self.source_identity
            || self.source_identity[0] != self.victim_identity[0]
        {
            return Err(refused());
        }
        let mut canonical = self.clone();
        canonical.signature.clear();
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&canonical).map_err(io::Error::other)?);
        if bytes.len() > LIMIT {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Decode only closed bounded observations, never a live completion owner.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > LIMIT {
            return Err(refused());
        }
        let receipt: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        receipt.signing_bytes()?;
        if receipt.signature.len() != 64 {
            return Err(refused());
        }
        Ok(receipt)
    }
    /// # Errors
    /// Checks the genuine prepared signature and exact preparation, enrollment
    /// and commit signatures. Caller must ALSO retain/check the actual source
    /// pin and compare capture/native identities at the effect boundary.
    pub fn verify(&self, prepared: &PreparedReceipt, verifier: &VerifierClaims) -> io::Result<()> {
        prepared.verify(verifier)?;
        if self.preparation().signing_bytes()? != prepared.signing_bytes()? {
            return Err(refused());
        }
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| refused())?;
        VerifyingKey::from_bytes(&verifier.device)
            .map_err(io::Error::other)?
            .verify(&self.signing_bytes()?, &Signature::from_bytes(&signature))
            .map_err(io::Error::other)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    fn fixture() -> (CommittedReceipt, PreparedReceipt, VerifierClaims) {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let uuid = |value| uuid::Uuid::from_u128(value).to_string();
        let verifier = VerifierClaims {
            schema: 1,
            source_owner: [2; 32],
            source_operation: uuid(1),
            person,
            personal_room: uuid(2),
            device: signer.verifying_key().to_bytes(),
        };
        let mut prepared = PreparedReceipt {
            schema: 1,
            operation_id: uuid(3),
            source_owner: [2; 32],
            intent_digest: [4; 32],
            personal_room: uuid(2),
            person,
            device: verifier.device,
            transaction_digest: [5; 32],
            archive_binding_digest: [6; 32],
            signature: Vec::new(),
        };
        prepared.signature = signer
            .sign(&prepared.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        let mut commit = CommittedReceipt {
            schema: 1,
            operation_id: uuid(3),
            source_owner: [2; 32],
            intent_digest: [4; 32],
            personal_room: uuid(2),
            person,
            device: verifier.device,
            transaction_digest: [5; 32],
            archive_binding_digest: [6; 32],
            source_identity: [8, 9],
            victim_identity: [8, 10],
            destination_identity: [8, 9],
            capture_id: uuid(4),
            journal_commit_digest: [11; 32],
            source_record_id: uuid(5),
            source_version_id: uuid(6),
            victim_record_id: uuid(7),
            victim_version_id: uuid(8),
            signature: Vec::new(),
        };
        commit.signature = signer
            .sign(&commit.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        (commit, prepared, verifier)
    }
    #[test]
    fn signed_commit_binds_exact_preparation_capture_and_canonical_records() {
        // Crypto/codec qualification only; these fixtures mint no source grant,
        // immutable bytes, actual pin, journal durability or daemon enrollment.
        let (receipt, prepared, verifier) = fixture();
        receipt.verify(&prepared, &verifier).unwrap();
        for field in [
            "capture",
            "transaction",
            "archive",
            "destination",
            "record",
            "journal",
        ] {
            let mut changed = receipt.clone();
            match field {
                "capture" => changed.capture_id = uuid::Uuid::from_u128(20).to_string(),
                "transaction" => changed.transaction_digest[0] ^= 1,
                "archive" => changed.archive_binding_digest[0] ^= 1,
                "destination" => changed.destination_identity[1] += 1,
                "record" => changed.source_record_id = uuid::Uuid::from_u128(21).to_string(),
                _ => changed.journal_commit_digest[0] ^= 1,
            }
            assert!(
                changed.verify(&prepared, &verifier).is_err(),
                "changed {field}"
            );
        }
        let mut other = prepared;
        other.operation_id = uuid::Uuid::from_u128(22).to_string();
        assert!(receipt.verify(&other, &verifier).is_err());
    }
    #[test]
    fn commit_cannot_decode_prepare_or_add_claimed_content_history() {
        let (receipt, prepared, _) = fixture();
        assert!(CommittedReceipt::decode(&serde_json::to_vec(&prepared).unwrap()).is_err());
        assert_eq!(
            CommittedReceipt::decode(&serde_json::to_vec(&receipt).unwrap()).unwrap(),
            receipt
        );
        let mut claimed = serde_json::to_value(&receipt).unwrap();
        claimed["content_hash"] = "claimed".into();
        assert!(CommittedReceipt::decode(&serde_json::to_vec(&claimed).unwrap()).is_err());
        assert!(CommittedReceipt::decode(&vec![b' '; LIMIT + 1]).is_err());
    }
}
