//! Purpose-separated credentials for externally approved moderation operations.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::{
    ModerationActorRole, ModerationMutation, ModerationWireError, SignedModerationMutation,
};

pub(crate) const MUTATION_DOMAIN: &[u8] = b"tilo-registry-moderation-v2\0";
pub(crate) const BINDING_DOMAIN: &[u8] = b"tilo-registry-moderation-credential-v2\0";

/// Exact registry endpoint and owner approved by the actor. No URL rewriting is
/// performed between approval, signing and server comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModerationDestination {
    pub base_url: String,
    pub owner_pubkey_hex: String,
}

/// The complete JSON action payload presented for approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalModerationOperation {
    pub destination: ModerationDestination,
    pub request_id: String,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub mutation: ModerationMutation,
}

impl ExternalModerationOperation {
    /// Stable action identifier; neither role nor action is caller-selected.
    #[must_use]
    pub fn action(&self) -> &'static str {
        match self.mutation {
            ModerationMutation::FileReport { .. } => "registry.file_report",
            ModerationMutation::PublisherReply { .. } => "registry.publisher_reply",
            ModerationMutation::PublisherAppeal { .. } => "registry.publisher_appeal",
            ModerationMutation::ReviewerDecision { .. } => "registry.reviewer_decision",
        }
    }

    pub(crate) fn role(&self) -> ModerationActorRole {
        match self.mutation {
            ModerationMutation::FileReport { .. } => ModerationActorRole::Reporter,
            ModerationMutation::PublisherReply { .. }
            | ModerationMutation::PublisherAppeal { .. } => ModerationActorRole::Publisher,
            ModerationMutation::ReviewerDecision { .. } => ModerationActorRole::Reviewer,
        }
    }

    /// Check structural bounds before approval or signing. The receiving server
    /// additionally checks its own destination and current wall clock.
    ///
    /// # Errors
    /// Returns the invalid field's constraint.
    pub fn validate(&self) -> Result<(), String> {
        let url = &self.destination.base_url;
        let authority = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .ok_or("registry URL must use HTTP or HTTPS")?;
        if url.len() > 2048
            || authority.is_empty()
            || authority.starts_with('/')
            || url
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            || url.contains(['?', '#', '@', '\\'])
        {
            return Err("invalid registry base URL".into());
        }
        digest(&self.destination.owner_pubkey_hex)?;
        identifier(&self.request_id)?;
        if self.expires_at_unix <= self.issued_at_unix
            || self.expires_at_unix.saturating_sub(self.issued_at_unix) > 86_400
        {
            return Err("validity must be positive and at most one day".into());
        }
        match &self.mutation {
            ModerationMutation::FileReport {
                package_id,
                category,
                summary,
                evidence_digest_hex,
            } => {
                identifier(package_id)?;
                text(category, 1, 64)?;
                text(summary, 8, 4096)?;
                digest(evidence_digest_hex)?;
            }
            ModerationMutation::PublisherReply {
                case_id,
                message,
                evidence_digest_hex,
            } => {
                identifier(case_id)?;
                text(message, 1, 8192)?;
                if let Some(value) = evidence_digest_hex {
                    digest(value)?;
                }
            }
            ModerationMutation::PublisherAppeal {
                case_id,
                reason,
                evidence_digest_hex,
            } => {
                identifier(case_id)?;
                text(reason, 8, 8192)?;
                if let Some(value) = evidence_digest_hex {
                    digest(value)?;
                }
            }
            ModerationMutation::ReviewerDecision {
                case_id, reason, ..
            } => {
                identifier(case_id)?;
                text(reason, 1, 4096)?;
            }
        }
        Ok(())
    }
}

fn identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err("invalid identifier".into());
    }
    Ok(())
}
fn digest(value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid 32-byte hex value".into());
    }
    Ok(())
}
fn text(value: &str, min: usize, max: usize) -> Result<(), String> {
    if !(min..=max).contains(&value.chars().count()) {
        return Err("text outside allowed bounds".into());
    }
    Ok(())
}

/// Identity-signed delegation to a destination-specific moderation key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModerationCredential {
    pub public_key_hex: String,
    pub binding_signature_hex: String,
}

pub(crate) fn binding_message(
    request: &SignedModerationMutation,
) -> Result<Vec<u8>, ModerationWireError> {
    let credential = request
        .credential
        .as_ref()
        .ok_or(ModerationWireError::InvalidSignature)?;
    let destination = request
        .destination
        .as_ref()
        .ok_or(ModerationWireError::InvalidSignature)?;
    let mut bytes = BINDING_DOMAIN.to_vec();
    bytes.extend(
        serde_json::to_vec(&(
            2u16,
            &request.actor_pubkey_hex,
            destination,
            &credential.public_key_hex,
        ))
        .map_err(|_| ModerationWireError::Encoding)?,
    );
    Ok(bytes)
}

/// Sign an approved operation with a purpose- and destination-specific key.
/// Only this fixed credential binding is signed by the identity key; arbitrary
/// action payloads are never signed in the identity key's protocol domain.
///
/// # Errors
/// Returns validation or canonical encoding errors.
pub fn sign_external_moderation(
    operation: &ExternalModerationOperation,
    identity: &SigningKey,
) -> Result<SignedModerationMutation, ModerationWireError> {
    operation
        .validate()
        .map_err(ModerationWireError::InvalidOperation)?;
    let destination =
        serde_json::to_vec(&operation.destination).map_err(|_| ModerationWireError::Encoding)?;
    let secret = blake3::derive_key(
        "tilo.registry.moderation.credential.v2",
        &identity.to_bytes(),
    );
    let key = SigningKey::from_bytes(blake3::keyed_hash(&secret, &destination).as_bytes());
    let mut request = SignedModerationMutation {
        protocol: 2,
        request_id: operation.request_id.clone(),
        issued_at_unix: operation.issued_at_unix,
        expires_at_unix: operation.expires_at_unix,
        actor_role: operation.role(),
        actor_pubkey_hex: hex::encode(identity.verifying_key().to_bytes()),
        mutation: operation.mutation.clone(),
        destination: Some(operation.destination.clone()),
        credential: Some(ModerationCredential {
            public_key_hex: hex::encode(key.verifying_key().to_bytes()),
            binding_signature_hex: String::new(),
        }),
        signature_hex: String::new(),
    };
    let binding = hex::encode(identity.sign(&binding_message(&request)?).to_bytes());
    if let Some(credential) = &mut request.credential {
        credential.binding_signature_hex = binding;
    }
    request.signature_hex = hex::encode(
        key.sign(&crate::moderation_signing_message(&request)?)
            .to_bytes(),
    );
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sign_moderation_mutation, verify_moderation_mutation};
    use ed25519_dalek::{Signature, VerifyingKey};

    fn operation() -> ExternalModerationOperation {
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "https://registry.example/catalogue".into(),
                owner_pubkey_hex: hex::encode(
                    SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
                ),
            },
            request_id: "report-one".into(),
            issued_at_unix: 100,
            expires_at_unix: 200,
            mutation: ModerationMutation::FileReport {
                package_id: "so.tilo.notes".into(),
                category: "security".into(),
                summary: "Unexpected outbound connections".into(),
                evidence_digest_hex: "ab".repeat(32),
            },
        }
    }

    #[test]
    fn moderation_v2_binds_identity_destination_and_exact_operation() {
        let identity = SigningKey::from_bytes(&[7; 32]);
        let operation = operation();
        let signed = sign_external_moderation(&operation, &identity).unwrap();
        assert_eq!(signed.protocol, 2);
        assert_eq!(signed.external_operation().unwrap(), operation);
        assert_eq!(
            signed.actor_pubkey_hex,
            hex::encode(identity.verifying_key().to_bytes())
        );
        assert_ne!(
            signed.credential.as_ref().unwrap().public_key_hex,
            signed.actor_pubkey_hex
        );
        verify_moderation_mutation(&signed).unwrap();
        let json = serde_json::to_vec(&operation).unwrap();
        assert_eq!(
            serde_json::from_slice::<ExternalModerationOperation>(&json).unwrap(),
            operation
        );
        let signed_json = serde_json::to_vec(&signed).unwrap();
        verify_moderation_mutation(&serde_json::from_slice(&signed_json).unwrap()).unwrap();
        for field in [
            "actor",
            "role",
            "url",
            "owner",
            "request",
            "issued",
            "expires",
            "body",
            "binding",
            "credential",
            "protocol",
        ] {
            let mut changed = signed.clone();
            match field {
                "actor" => {
                    changed.actor_pubkey_hex =
                        hex::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes());
                }
                "role" => changed.actor_role = ModerationActorRole::Reviewer,
                "url" => changed.destination.as_mut().unwrap().base_url.push('/'),
                "owner" => changed.destination.as_mut().unwrap().owner_pubkey_hex = "aa".repeat(32),
                "request" => changed.request_id.push('x'),
                "issued" => changed.issued_at_unix += 1,
                "expires" => changed.expires_at_unix += 1,
                "body" => {
                    if let ModerationMutation::FileReport { summary, .. } = &mut changed.mutation {
                        summary.push('x');
                    }
                }
                "binding" => {
                    changed.credential.as_mut().unwrap().binding_signature_hex = "00".repeat(64);
                }
                "credential" => {
                    changed.credential.as_mut().unwrap().public_key_hex =
                        changed.actor_pubkey_hex.clone();
                }
                "protocol" => changed.protocol = 1,
                _ => unreachable!(),
            }
            assert!(verify_moderation_mutation(&changed).is_err(), "{field}");
        }
        let mut wrong_key = signed;
        wrong_key.signature_hex = hex::encode(
            identity
                .sign(&crate::moderation_signing_message(&wrong_key).unwrap())
                .to_bytes(),
        );
        assert!(verify_moderation_mutation(&wrong_key).is_err());
    }

    #[test]
    fn moderation_credential_is_not_an_identity_signature_or_cross_destination_key() {
        let identity = SigningKey::from_bytes(&[7; 32]);
        let operation = operation();
        let signed = sign_external_moderation(&operation, &identity).unwrap();
        let message = crate::moderation_signing_message(&signed).unwrap();
        let signature =
            Signature::from_slice(&hex::decode(&signed.signature_hex).unwrap()).unwrap();
        assert!(
            identity
                .verifying_key()
                .verify_strict(&message, &signature)
                .is_err()
        );
        let credential = signed.credential.as_ref().unwrap();
        let key = VerifyingKey::from_bytes(
            &hex::decode(&credential.public_key_hex)
                .unwrap()
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let binding =
            Signature::from_slice(&hex::decode(&credential.binding_signature_hex).unwrap())
                .unwrap();
        let binding_message = binding_message(&signed).unwrap();
        identity
            .verifying_key()
            .verify_strict(&binding_message, &binding)
            .unwrap();
        assert!(key.verify_strict(&binding_message, &binding).is_err());
        assert!(
            identity
                .verifying_key()
                .verify_strict(&message, &binding)
                .is_err()
        );
        let mut other = operation;
        other.destination.base_url.push('/');
        let other = sign_external_moderation(&other, &identity).unwrap();
        assert_ne!(
            other.credential.unwrap().public_key_hex,
            credential.public_key_hex
        );
    }

    #[test]
    fn moderation_v2_canonical_json_and_transparency_survive_reencoding() {
        let signed =
            sign_external_moderation(&operation(), &SigningKey::from_bytes(&[7; 32])).unwrap();
        // Value's sorted object keys differ from the wire structs' declaration order.
        let json = serde_json::to_string_pretty(&serde_json::to_value(&signed).unwrap()).unwrap();
        let decoded = serde_json::from_str(&json).unwrap();
        assert_eq!(
            crate::moderation_signing_message(&signed),
            crate::moderation_signing_message(&decoded)
        );
        verify_moderation_mutation(&decoded).unwrap();
        let mut event = report_event(&decoded);
        event.record_hash_hex = crate::transparency_event_hash(&event).unwrap();
        let head = event.record_hash_hex.clone();
        let owner = event
            .signed_mutation
            .destination
            .as_ref()
            .unwrap()
            .owner_pubkey_hex
            .clone();
        crate::derive_verified_case(&[event.clone()], &event.case_id, &owner, &head).unwrap();
        assert!(
            crate::derive_verified_case(&[event.clone()], &event.case_id, &"aa".repeat(32), &head)
                .is_err()
        );
        event
            .signed_mutation
            .destination
            .as_mut()
            .unwrap()
            .base_url
            .push('/');
        // Rehashing the public log cannot authenticate a changed signed destination.
        event.record_hash_hex = crate::transparency_event_hash(&event).unwrap();
        assert!(crate::verify_transparency_chain(&[event], "").is_err());
    }

    #[test]
    fn moderation_json_rejects_unapproved_fields_and_invalid_bounds() {
        let operation = operation();
        for path in ["operation", "destination", "mutation"] {
            let mut json = serde_json::to_value(&operation).unwrap();
            let target = match path {
                "destination" => &mut json["destination"],
                "mutation" => &mut json["mutation"],
                _ => &mut json,
            };
            target
                .as_object_mut()
                .unwrap()
                .insert("unapproved".into(), true.into());
            assert!(serde_json::from_value::<ExternalModerationOperation>(json).is_err());
        }
        let mut invalid = operation;
        invalid.expires_at_unix = invalid.issued_at_unix;
        assert!(invalid.validate().is_err());
        assert!(sign_external_moderation(&invalid, &SigningKey::from_bytes(&[7; 32])).is_err());
    }

    #[test]
    fn moderation_v1_remains_verifiable_without_v2_fields() {
        let operation = operation();
        let signed = sign_moderation_mutation(
            operation.request_id,
            100,
            200,
            ModerationActorRole::Reporter,
            operation.mutation,
            &SigningKey::from_bytes(&[7; 32]),
        )
        .unwrap();
        let json = serde_json::to_value(&signed).unwrap();
        assert!(json.get("credential").is_none());
        assert!(json.get("destination").is_none());
        let decoded: SignedModerationMutation = serde_json::from_value(json).unwrap();
        verify_moderation_mutation(&decoded).unwrap();
        let legacy_payload = postcard::to_stdvec(&(
            1u16,
            &signed.request_id,
            signed.issued_at_unix,
            signed.expires_at_unix,
            signed.actor_role,
            &signed.actor_pubkey_hex,
            &signed.mutation,
        ))
        .unwrap();
        let mut legacy_digest = blake3::Hasher::new();
        legacy_digest.update(crate::MODERATION_DOMAIN);
        legacy_digest.update(&legacy_payload);
        SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .verify_strict(
                legacy_digest.finalize().as_bytes(),
                &Signature::from_slice(&hex::decode(&signed.signature_hex).unwrap()).unwrap(),
            )
            .unwrap();
        // Postcard structs are positional: omitted v2 fields must leave the
        // historic event preimage byte-for-byte unchanged.
        let mut event = report_event(&signed);
        let legacy_request = (
            signed.protocol,
            &signed.request_id,
            signed.issued_at_unix,
            signed.expires_at_unix,
            signed.actor_role,
            &signed.actor_pubkey_hex,
            &signed.mutation,
            &signed.signature_hex,
        );
        let legacy_event = (
            event.sequence,
            &event.case_id,
            &event.package_id,
            &event.publisher_pubkey_hex,
            event.recorded_at_unix,
            event.actor_role,
            &event.actor_pubkey_hex,
            &event.request_id,
            &event.event,
            &event.actor_signature_hex,
            legacy_request,
            &event.previous_hash_hex,
            &event.record_hash_hex,
        );
        let bytes = postcard::to_stdvec(&legacy_event).unwrap();
        event.record_hash_hex = blake3::hash(&bytes).to_hex().to_string();
        assert_eq!(
            crate::transparency_event_hash(&event).unwrap(),
            event.record_hash_hex
        );
        crate::derive_verified_case(
            &[event.clone()],
            &event.case_id,
            &hex::encode(SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes()),
            &event.record_hash_hex,
        )
        .unwrap();
        let mut unknown = signed;
        unknown.protocol = 99;
        assert!(verify_moderation_mutation(&unknown).is_err());
    }

    fn report_event(signed: &SignedModerationMutation) -> crate::ModerationEvent {
        crate::ModerationEvent {
            sequence: 1,
            case_id: crate::moderation_case_id("so.tilo.notes", &signed.request_id),
            package_id: "so.tilo.notes".into(),
            publisher_pubkey_hex: "ab".repeat(32),
            recorded_at_unix: 110,
            actor_role: signed.actor_role,
            actor_pubkey_hex: signed.actor_pubkey_hex.clone(),
            request_id: signed.request_id.clone(),
            event: crate::ModerationEventKind::ReportFiled {
                category: "security".into(),
                summary: "Unexpected outbound connections".into(),
                evidence_digest_hex: "ab".repeat(32),
            },
            actor_signature_hex: signed.signature_hex.clone(),
            signed_mutation: signed.clone(),
            previous_hash_hex: String::new(),
            record_hash_hex: String::new(),
        }
    }
}
