//! Closed external replacement receipts. Signatures authenticate expected
//! intent only; they never prove native completion or mint a filesystem grant.
mod commit;
pub mod daemon_control;
pub use commit::CommittedReceipt;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::io;
const DOMAIN: &[u8] = b"tilo-retained-external-replacement-prepare-v1\0";
pub use tilo_fs_client::external_history::LIMIT;
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}
pub use runtime_portal::retained_role::NativeHistoryVerifierRecord as VerifierClaims;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedReceipt {
    pub schema: u32,
    pub operation_id: String,
    pub source_owner: [u8; 32],
    pub intent_digest: [u8; 32],
    pub personal_room: String,
    pub person: [u8; 32],
    pub device: [u8; 32],
    pub transaction_digest: [u8; 32],
    /// Protected reserved archive PLAN, before effects. This is not a hash
    /// of captured bytes or a claim that an archive already exists.
    pub archive_binding_digest: [u8; 32],
    pub signature: Vec<u8>,
}
#[derive(Serialize)]
struct SigningView<'a> {
    schema: u32,
    operation_id: &'a str,
    source_owner: [u8; 32],
    intent_digest: [u8; 32],
    personal_room: &'a str,
    person: [u8; 32],
    device: [u8; 32],
    transaction_digest: [u8; 32],
    archive_binding_digest: [u8; 32],
}
impl PreparedReceipt {
    pub fn signing_bytes(&self) -> io::Result<Vec<u8>> {
        if self.schema != 1
            || !id(&self.operation_id)
            || !id(&self.personal_room)
            || self.source_owner == [0; 32]
            || self.intent_digest == [0; 32]
            || self.person == [0; 32]
            || self.device == [0; 32]
            || self.transaction_digest == [0; 32]
            || self.archive_binding_digest == [0; 32]
            || VerifyingKey::from_bytes(&self.person).is_err()
            || VerifyingKey::from_bytes(&self.device).is_err()
        {
            return Err(refused());
        }
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(
            postcard::to_allocvec(&SigningView {
                schema: self.schema,
                operation_id: &self.operation_id,
                source_owner: self.source_owner,
                intent_digest: self.intent_digest,
                personal_room: &self.personal_room,
                person: self.person,
                device: self.device,
                transaction_digest: self.transaction_digest,
                archive_binding_digest: self.archive_binding_digest,
            })
            .map_err(io::Error::other)?,
        );
        if bytes.len() > LIMIT {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Decode an expected intent, never observed native completion.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > LIMIT {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        value.signing_bytes()?;
        if value.signature.len() != 64 {
            return Err(refused());
        }
        Ok(value)
    }
    /// # Errors
    /// Verifies signature/equality only. The caller must obtain `verifier`
    /// from its private authenticated source custody and compare its actual
    /// held reserved operation and source-minted intent before any effects.
    pub fn verify(&self, verifier: &VerifierClaims) -> io::Result<()> {
        if !verifier.valid()
            || self.person != verifier.person
            || self.device != verifier.device
            || self.personal_room != verifier.personal_room
            || self.source_owner != verifier.source_owner
        {
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

/// Bounded daemon challenge over the accepted existing storage connection.
/// Certificate bytes are not enrollment; the caller independently validates
/// the fixed actual daemon task, current certificate/revocations and signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentChallenge {
    pub schema: u32,
    pub nonce: [u8; 32],
    pub certificate: Vec<u8>,
    pub revocations: Vec<u8>,
}
impl EnrollmentChallenge {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && self.nonce != [0; 32]
            && !self.certificate.is_empty()
            && self.certificate.len() <= 16 * 1024
            && !self.revocations.is_empty()
            && self.revocations.len() <= 256 * 1024
    }
}
/// Exact descriptive context delivered from the real helper's protected source
/// custody. The receiver additionally consumes the root-sealed descriptor and
/// independently authenticates the fresh Shell proof endpoint; JSON is no grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentContext {
    pub schema: u32,
    pub nonce: [u8; 32],
    pub verifier: VerifierClaims,
    /// Canonical `RetainedLaunchBinding` JSON, independently decoded/compared by
    /// the daemon. Keeping the lower codec declarative avoids a dependency cycle.
    pub person_binding: Vec<u8>,
}
impl EnrollmentContext {
    /// # Errors
    /// Refuses malformed or oversized descriptive enrollment context.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if self.schema != 1
            || self.nonce == [0; 32]
            || !self.verifier.valid()
            || self.person_binding.is_empty()
            || self.person_binding.len() > 4096
        {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > 32 * 1024 {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses malformed, unknown or oversized fields.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 32 * 1024 {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if value.encode()? != bytes {
            return Err(refused());
        }
        Ok(value)
    }
    /// # Errors
    /// Requires the same valid canonical bounded context.
    pub fn digest(&self) -> io::Result<[u8; 32]> {
        Ok(*blake3::hash(&self.encode()?).as_bytes())
    }
}
/// Closed control on the same enrolled daemon connection. Replacement commands
/// route private held capabilities; no path, grant or observation tuple travels
/// in these commands. Each refresh has a new requester nonce.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EnrollmentCommand {
    RetireReplacement {
        request_id: String,
        successor_request_id: String,
        operation_id: String,
        transaction_digest: [u8; 32],
    },
    RetireHistoryRead {
        request_id: String,
    },
    AdoptHistoryRead {
        request_id: String,
    },
    QueryHistory {
        request_id: String,
        query: String,
    },
    StartReplacement {
        request_id: String,
    },
    PollReplacement {
        request_id: String,
    },
    CancelReplacement {
        request_id: String,
    },
    Refresh {
        nonce: [u8; 32],
    },
    Retire,
}
impl EnrollmentCommand {
    /// # Errors
    /// Refuses malformed, unknown or oversized fields.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if matches!(&value, Self::Refresh { nonce } if *nonce == [0; 32]) {
            return Err(refused());
        }
        if let Self::RetireHistoryRead { request_id }
        | Self::AdoptHistoryRead { request_id }
        | Self::QueryHistory { request_id, .. }
        | Self::StartReplacement { request_id }
        | Self::PollReplacement { request_id }
        | Self::CancelReplacement { request_id } = &value
            && !id(request_id)
        {
            return Err(refused());
        }
        if let Self::RetireReplacement {
            request_id,
            successor_request_id,
            operation_id,
            transaction_digest,
        } = &value
            && (!id(request_id)
                || !id(successor_request_id)
                || !id(operation_id)
                || request_id == successor_request_id
                || *transaction_digest == [0; 32])
        {
            return Err(refused());
        }
        if let Self::QueryHistory { query, .. } = &value
            && !read_control::valid_query(query)
        {
            return Err(refused());
        }
        Ok(value)
    }
}
pub mod read_control;
pub mod replacement_control;
/// Device proof of possession for an independently admitted enrollment. This
/// describes a verifier; it is never a prepared replacement or save completion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledVerifier {
    pub schema: u32,
    pub daemon_nonce: [u8; 32],
    pub requester_nonce: [u8; 32],
    pub context_digest: [u8; 32],
    pub verifier: VerifierClaims,
    pub signature: Vec<u8>,
}
impl EnrolledVerifier {
    /// # Errors
    /// Refuses malformed nonces, context hash and verifier vocabulary.
    pub fn signing_bytes(&self) -> io::Result<Vec<u8>> {
        if self.schema != 1
            || self.daemon_nonce == [0; 32]
            || self.requester_nonce == [0; 32]
            || self.context_digest == [0; 32]
            || !self.verifier.valid()
        {
            return Err(refused());
        }
        let mut bytes = b"tilo-retained-native-history-enrollment-v1\0".to_vec();
        bytes.extend(
            postcard::to_allocvec(&(
                self.schema,
                self.daemon_nonce,
                self.requester_nonce,
                self.context_digest,
                &self.verifier,
            ))
            .map_err(io::Error::other)?,
        );
        if bytes.len() > LIMIT {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses malformed, unknown or oversized fields.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 8192 {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        value.signing_bytes()?;
        if value.signature.len() != 64 {
            return Err(refused());
        }
        Ok(value)
    }
    /// Call only after independently validating the actual daemon role and
    /// current certified device. Matching wire values never establish authority.
    /// # Errors
    /// Refuses changed context/nonces/device or invalid signature.
    pub fn verify(
        &self,
        context: &EnrollmentContext,
        challenge: &EnrollmentChallenge,
        certified_device: &[u8; 32],
    ) -> io::Result<()> {
        self.verify_nonce(context, challenge, context.nonce, certified_device)
    }
    /// Verify a new refresh challenge without rewriting the originally enrolled
    /// context or treating an earlier reply as fresh certification.
    /// # Errors
    /// Refuses zero/replayed requester nonce, changed context/daemon or signature.
    pub fn verify_refresh(
        &self,
        context: &EnrollmentContext,
        challenge: &EnrollmentChallenge,
        requested_nonce: [u8; 32],
        certified_device: &[u8; 32],
    ) -> io::Result<()> {
        self.verify_nonce(context, challenge, requested_nonce, certified_device)
    }
    fn verify_nonce(
        &self,
        context: &EnrollmentContext,
        challenge: &EnrollmentChallenge,
        requested_nonce: [u8; 32],
        certified_device: &[u8; 32],
    ) -> io::Result<()> {
        if requested_nonce == [0; 32]
            || !challenge.valid()
            || self.daemon_nonce != challenge.nonce
            || self.requester_nonce != requested_nonce
            || self.context_digest != context.digest()?
            || self.verifier != context.verifier
            || &self.verifier.device != certified_device
        {
            return Err(refused());
        }
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| refused())?;
        VerifyingKey::from_bytes(certified_device)
            .map_err(io::Error::other)?
            .verify(&self.signing_bytes()?, &Signature::from_bytes(&signature))
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn signed_refresh_requires_fresh_nonce_and_original_context() {
        let (_, verifier) = fixture();
        let device = SigningKey::from_bytes(&[7; 32]);
        let context = EnrollmentContext {
            schema: 1,
            nonce: [11; 32],
            verifier,
            person_binding: b"{}".to_vec(),
        };
        let challenge = EnrollmentChallenge {
            schema: 1,
            nonce: [12; 32],
            certificate: vec![1],
            revocations: vec![2],
        };
        let fresh = [13; 32];
        let mut reply = EnrolledVerifier {
            schema: 1,
            daemon_nonce: challenge.nonce,
            requester_nonce: fresh,
            context_digest: context.digest().unwrap(),
            verifier: context.verifier.clone(),
            signature: Vec::new(),
        };
        reply.signature = device
            .sign(&reply.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        let key = device.verifying_key().to_bytes();
        reply
            .verify_refresh(&context, &challenge, fresh, &key)
            .unwrap();
        assert!(reply.verify(&context, &challenge, &key).is_err());
        assert!(
            reply
                .verify_refresh(&context, &challenge, [14; 32], &key)
                .is_err()
        );
        assert!(
            reply
                .verify_refresh(&context, &challenge, [0; 32], &key)
                .is_err()
        );
        let mut changed = context.clone();
        changed.nonce = fresh;
        assert!(
            reply
                .verify_refresh(&changed, &challenge, fresh, &key)
                .is_err()
        );
        changed = context.clone();
        changed.verifier.source_owner = [15; 32];
        assert!(
            reply
                .verify_refresh(&changed, &challenge, fresh, &key)
                .is_err()
        );
        let mut changed_challenge = challenge.clone();
        changed_challenge.nonce = [16; 32];
        assert!(
            reply
                .verify_refresh(&context, &changed_challenge, fresh, &key)
                .is_err()
        );
        assert!(
            reply
                .verify_refresh(
                    &context,
                    &challenge,
                    fresh,
                    &SigningKey::from_bytes(&[17; 32]).verifying_key().to_bytes()
                )
                .is_err()
        );
    }
    fn fixture() -> (PreparedReceipt, VerifierClaims) {
        let device = SigningKey::from_bytes(&[7; 32]);
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let verifier = VerifierClaims {
            schema: 1,
            source_owner: [2; 32],
            source_operation: "11111111-1111-4111-8111-111111111111".into(),
            person,
            personal_room: "22222222-2222-4222-8222-222222222222".into(),
            device: device.verifying_key().to_bytes(),
        };
        let mut receipt = PreparedReceipt {
            schema: 1,
            operation_id: "33333333-3333-4333-8333-333333333333".into(),
            source_owner: verifier.source_owner,
            intent_digest: [3; 32],
            personal_room: verifier.personal_room.clone(),
            person,
            device: verifier.device,
            transaction_digest: [4; 32],
            archive_binding_digest: [5; 32],
            signature: Vec::new(),
        };
        receipt.signature = device
            .sign(&receipt.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        (receipt, verifier)
    }
    // Lower codec/signature qualification only: no sealed descriptor, source
    // grant, native reservation or completed history is manufactured here.
    #[test]
    fn signed_intent_refuses_changed_reserved_operation_and_archive_plan() {
        let (receipt, verifier) = fixture();
        receipt.verify(&verifier).unwrap();
        let mut changed = receipt.clone();
        changed.operation_id = "44444444-4444-4444-8444-444444444444".into();
        assert!(changed.verify(&verifier).is_err());
        changed = receipt.clone();
        changed.archive_binding_digest[0] ^= 1;
        assert!(changed.verify(&verifier).is_err());
        changed = receipt;
        changed.intent_digest[0] ^= 1;
        assert!(changed.verify(&verifier).is_err());
    }
    #[test]
    fn recipient_and_source_cannot_be_substituted() {
        let (receipt, verifier) = fixture();
        let mut changed = verifier.clone();
        changed.source_owner[0] ^= 1;
        assert!(receipt.verify(&changed).is_err());
        changed = verifier.clone();
        changed.person[0] ^= 1;
        assert!(receipt.verify(&changed).is_err());
        changed = verifier;
        changed.personal_room = "55555555-5555-4555-8555-555555555555".into();
        assert!(receipt.verify(&changed).is_err());
    }
    #[test]
    fn closed_bounded_receipt_refuses_unknown_fields_and_invalid_ids() {
        let (mut receipt, _) = fixture();
        let encoded = serde_json::to_vec(&receipt).unwrap();
        PreparedReceipt::decode(&encoded).unwrap();
        let mut unknown = serde_json::to_value(&receipt).unwrap();
        unknown["completed"] = true.into();
        assert!(PreparedReceipt::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
        assert!(PreparedReceipt::decode(&vec![b' '; LIMIT + 1]).is_err());
        receipt.operation_id = uuid::Uuid::nil().to_string();
        assert!(receipt.signing_bytes().is_err());
        receipt.operation_id = "33333333-3333-4333-8333-333333333333".into();
        receipt.device = [0; 32];
        assert!(receipt.signing_bytes().is_err());
    }
    #[test]
    fn enrolled_verifier_binds_both_nonces_exact_context_and_certified_device() {
        let (receipt, verifier) = fixture();
        let device = SigningKey::from_bytes(&[7; 32]);
        let context = EnrollmentContext {
            schema: 1,
            nonce: [11; 32],
            verifier,
            person_binding: br#"{"opaque_context":"lower_codec_only"}"#.to_vec(),
        };
        let challenge = EnrollmentChallenge {
            schema: 1,
            nonce: [12; 32],
            certificate: vec![1],
            revocations: vec![2],
        };
        // This tests proof-of-possession framing only, without claiming that
        // the deliberately opaque lower-codec bytes are a real certificate.
        let mut enrolled = EnrolledVerifier {
            schema: 1,
            daemon_nonce: challenge.nonce,
            requester_nonce: context.nonce,
            context_digest: context.digest().unwrap(),
            verifier: context.verifier.clone(),
            signature: Vec::new(),
        };
        enrolled.signature = device
            .sign(&enrolled.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        enrolled
            .verify(&context, &challenge, &receipt.device)
            .unwrap();
        let mut altered = context.clone();
        altered.nonce[0] ^= 1;
        assert!(
            enrolled
                .verify(&altered, &challenge, &receipt.device)
                .is_err()
        );
        altered = context.clone();
        altered.person_binding.push(b' ');
        assert!(
            enrolled
                .verify(&altered, &challenge, &receipt.device)
                .is_err()
        );
        let mut changed = challenge;
        changed.nonce[0] ^= 1;
        assert!(
            enrolled
                .verify(&context, &changed, &receipt.device)
                .is_err()
        );
        assert!(enrolled.verify(&context, &changed, &[0; 32]).is_err());
        // Enrollment never decodes as an effect-authorizing prepared receipt.
        assert!(PreparedReceipt::decode(&serde_json::to_vec(&enrolled).unwrap()).is_err());
        let mut unknown = serde_json::to_value(&context).unwrap();
        unknown["save"] = true.into();
        assert!(EnrollmentContext::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }
}

pub use tilo_fs_client::external_history::{
    NativeReplacementPurpose, NativeReservationDescription, NativeReservationFacts,
    NativeReservationLocator,
};
