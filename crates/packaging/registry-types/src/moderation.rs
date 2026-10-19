//! Signed wire protocol for registry reports, replies, appeals and decisions.

use std::collections::HashMap;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The moderation protocol's one version: every signed mutation names it,
/// and a reader accepts exactly it (docs/architecture/compatibility.md). It is
/// part of what the actor's credential signs.
pub const MODERATION_PROTOCOL: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationActorRole {
    Reporter,
    Publisher,
    Reviewer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationDecision {
    NeedsInformation,
    NoAction,
    Quarantine,
    Restore,
}

impl ModerationDecision {
    /// The status a case is left in by this decision.
    #[must_use]
    pub fn status(self) -> ModerationCaseStatus {
        match self {
            Self::NeedsInformation => ModerationCaseStatus::NeedsInformation,
            Self::NoAction => ModerationCaseStatus::ClosedNoAction,
            Self::Quarantine => ModerationCaseStatus::Quarantined,
            Self::Restore => ModerationCaseStatus::Restored,
        }
    }
}

/// The id of the case a report opens: bound to the reported package and the
/// report's request, so a retried report names the same case.
#[must_use]
pub fn moderation_case_id(package_id: &str, request_id: &str) -> String {
    let hash = blake3::hash(format!("{package_id}:{request_id}").as_bytes());
    format!("case-{}", hex::encode(&hash.as_bytes()[..12]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationCaseStatus {
    Open,
    NeedsInformation,
    Appealed,
    ClosedNoAction,
    Quarantined,
    Restored,
}

/// A bounded mutation. Servers enforce the documented string limits before
/// verifying authorization or touching durable state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModerationMutation {
    FileReport {
        package_id: String,
        category: String,
        summary: String,
        /// BLAKE3 hex digest of an evidence envelope kept by the reporter.
        evidence_digest_hex: String,
    },
    PublisherReply {
        case_id: String,
        message: String,
        evidence_digest_hex: Option<String>,
    },
    PublisherAppeal {
        case_id: String,
        reason: String,
        evidence_digest_hex: Option<String>,
    },
    ReviewerDecision {
        case_id: String,
        decision: ModerationDecision,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedModerationMutation {
    pub protocol: u16,
    /// Caller-generated idempotency key. It is signed and accepted only once.
    pub request_id: String,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub actor_role: ModerationActorRole,
    pub actor_pubkey_hex: String,
    pub mutation: ModerationMutation,
    pub signature_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<crate::ModerationDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<crate::ModerationCredential>,
}

impl SignedModerationMutation {
    /// Recover the exact approved JSON action payload from a request.
    ///
    /// # Errors
    /// Returns an error for a request at another protocol or without a
    /// destination.
    pub fn external_operation(
        &self,
    ) -> Result<crate::ExternalModerationOperation, ModerationWireError> {
        if self.protocol != MODERATION_PROTOCOL {
            return Err(ModerationWireError::UnsupportedProtocol);
        }
        Ok(crate::ExternalModerationOperation {
            destination: self
                .destination
                .clone()
                .ok_or(ModerationWireError::InvalidSignature)?,
            request_id: self.request_id.clone(),
            issued_at_unix: self.issued_at_unix,
            expires_at_unix: self.expires_at_unix,
            mutation: self.mutation.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationCase {
    pub case_id: String,
    pub package_id: String,
    pub publisher_pubkey_hex: String,
    pub status: ModerationCaseStatus,
    pub opened_at_unix: u64,
    pub updated_at_unix: u64,
    pub latest_sequence: u64,
}

impl ModerationCase {
    /// The case `event` opens, open as of it.
    #[must_use]
    pub fn opened_by(event: &ModerationEvent) -> Self {
        Self {
            case_id: event.case_id.clone(),
            package_id: event.package_id.clone(),
            publisher_pubkey_hex: event.publisher_pubkey_hex.clone(),
            status: ModerationCaseStatus::Open,
            opened_at_unix: event.recorded_at_unix,
            updated_at_unix: event.recorded_at_unix,
            latest_sequence: event.sequence,
        }
    }

    /// The case as of `event`, one of its own: its time and sequence, and
    /// the status an appeal or a decision leaves it in.
    pub fn advance(&mut self, event: &ModerationEvent) {
        self.updated_at_unix = event.recorded_at_unix;
        self.latest_sequence = event.sequence;
        match &event.event {
            ModerationEventKind::ReviewerDecided { decision, .. } => {
                self.status = decision.status();
            }
            ModerationEventKind::PublisherAppealed { .. } => {
                self.status = ModerationCaseStatus::Appealed;
            }
            ModerationEventKind::ReportFiled { .. }
            | ModerationEventKind::PublisherReplied { .. } => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModerationEventKind {
    ReportFiled {
        category: String,
        summary: String,
        evidence_digest_hex: String,
    },
    PublisherReplied {
        message: String,
        evidence_digest_hex: Option<String>,
    },
    PublisherAppealed {
        reason: String,
        evidence_digest_hex: Option<String>,
    },
    ReviewerDecided {
        decision: ModerationDecision,
        reason: String,
    },
}

/// Public immutable record. The actor signature authenticates the mutation;
/// `previous_hash_hex` and `record_hash_hex` make deletion/reordering evident.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationEvent {
    pub sequence: u64,
    pub case_id: String,
    pub package_id: String,
    pub publisher_pubkey_hex: String,
    pub recorded_at_unix: u64,
    pub actor_role: ModerationActorRole,
    pub actor_pubkey_hex: String,
    pub request_id: String,
    pub event: ModerationEventKind,
    pub actor_signature_hex: String,
    /// Complete signed request, allowing independent public verification of
    /// the actor identity and exact intent rather than trusting a projection.
    pub signed_mutation: SignedModerationMutation,
    pub previous_hash_hex: String,
    pub record_hash_hex: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransparencyPage {
    pub events: Vec<ModerationEvent>,
    pub next_sequence: Option<u64>,
    pub chain_head_hex: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModerationWireError {
    #[error("invalid moderation operation: {0}")]
    InvalidOperation(String),
    #[error("unsupported moderation protocol")]
    UnsupportedProtocol,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("canonical encoding failed")]
    Encoding,
    #[error("transparency chain is invalid at sequence {0}")]
    InvalidChain(u64),
    #[error("transparency head does not match the owner-signed snapshot")]
    HeadMismatch,
    #[error("moderation actor is not authorized at sequence {0}")]
    UnauthorizedActor(u64),
    #[error("moderation case proof is incomplete or inconsistent")]
    InvalidCase,
}

/// Canonical digest covered by an actor signature.
///
/// # Errors
/// Returns an encoding error if the canonical payload cannot be serialized.
pub fn moderation_signing_message(
    request: &SignedModerationMutation,
) -> Result<[u8; 32], ModerationWireError> {
    let operation = request.external_operation()?;
    let bytes = serde_json::to_vec(&(
        request.protocol,
        operation.action(),
        &operation,
        request.actor_role,
        &request.actor_pubkey_hex,
        &request.credential,
    ))
    .map_err(|_| ModerationWireError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(crate::external_moderation::MUTATION_DOMAIN);
    hasher.update(&bytes);
    Ok(*hasher.finalize().as_bytes())
}

/// Verify an actor's public key and signature over the complete request.
///
/// # Errors
/// Returns a public-key, signature, or encoding error when verification fails.
pub fn verify_moderation_mutation(
    request: &SignedModerationMutation,
) -> Result<(), ModerationWireError> {
    let key_bytes: [u8; 32] = hex::decode(&request.actor_pubkey_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ModerationWireError::InvalidPublicKey)?;
    let signature = Signature::from_slice(
        &hex::decode(&request.signature_hex).map_err(|_| ModerationWireError::InvalidSignature)?,
    )
    .map_err(|_| ModerationWireError::InvalidSignature)?;
    let key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| ModerationWireError::InvalidPublicKey)?;
    let message = moderation_signing_message(request)?;
    let operation = request.external_operation()?;
    operation
        .validate()
        .map_err(ModerationWireError::InvalidOperation)?;
    if operation.role() != request.actor_role {
        return Err(ModerationWireError::InvalidSignature);
    }
    let credential = request
        .credential
        .as_ref()
        .ok_or(ModerationWireError::InvalidSignature)?;
    let binding = Signature::from_slice(
        &hex::decode(&credential.binding_signature_hex)
            .map_err(|_| ModerationWireError::InvalidSignature)?,
    )
    .map_err(|_| ModerationWireError::InvalidSignature)?;
    key.verify_strict(
        &crate::external_moderation::binding_message(request)?,
        &binding,
    )
    .map_err(|_| ModerationWireError::InvalidSignature)?;
    let bytes: [u8; 32] = hex::decode(&credential.public_key_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ModerationWireError::InvalidPublicKey)?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| ModerationWireError::InvalidPublicKey)?
        .verify_strict(&message, &signature)
        .map_err(|_| ModerationWireError::InvalidSignature)
}

/// Verify a contiguous page whose first `previous_hash_hex` is the caller's
/// trusted prior head (empty for sequence one). Returns the resulting head.
///
/// # Errors
/// Returns the first invalid sequence when any link, digest, signature or
/// signed-request projection is invalid.
pub fn verify_transparency_chain(
    events: &[ModerationEvent],
    prior_head_hex: &str,
) -> Result<String, ModerationWireError> {
    let mut prior = prior_head_hex.to_owned();
    let first_sequence = events.first().map_or(1, |event| event.sequence);
    if prior_head_hex.is_empty() && first_sequence != 1 {
        return Err(ModerationWireError::InvalidChain(first_sequence));
    }
    for (index, event) in events.iter().enumerate() {
        let expected_sequence =
            first_sequence.saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
        if event.sequence != expected_sequence
            || event.previous_hash_hex != prior
            || transparency_event_hash(event)? != event.record_hash_hex
            || verify_moderation_mutation(&event.signed_mutation).is_err()
            || event.request_id != event.signed_mutation.request_id
            || event.actor_role != event.signed_mutation.actor_role
            || event.actor_pubkey_hex != event.signed_mutation.actor_pubkey_hex
            || event.actor_signature_hex != event.signed_mutation.signature_hex
            || !event_matches_mutation(event)
        {
            return Err(ModerationWireError::InvalidChain(event.sequence));
        }
        prior.clone_from(&event.record_hash_hex);
    }
    Ok(prior)
}

/// Verify a complete moderation log against the head committed by an
/// owner-signed registry snapshot and derive one case from authenticated
/// events. The server's mutable `status` projection is never trusted.
///
/// # Errors
/// Returns an integrity, head, authorization, or case-shape error.
pub fn derive_verified_case(
    events: &[ModerationEvent],
    case_id: &str,
    owner_pubkey_hex: &str,
    committed_head_hex: &str,
) -> Result<ModerationCase, ModerationWireError> {
    let head = verify_transparency_chain(events, "")?;
    if head != committed_head_hex {
        return Err(ModerationWireError::HeadMismatch);
    }
    verify_moderation_authorization(events, owner_pubkey_hex)?;
    let mut case: Option<ModerationCase> = None;
    let mut saw_report = false;
    for event in events.iter().filter(|event| event.case_id == case_id) {
        if let Some(current) = &case
            && (current.package_id != event.package_id
                || !current
                    .publisher_pubkey_hex
                    .eq_ignore_ascii_case(&event.publisher_pubkey_hex))
        {
            return Err(ModerationWireError::InvalidCase);
        }
        match &event.event {
            ModerationEventKind::ReportFiled { .. } => {
                if saw_report || event.actor_role != ModerationActorRole::Reporter {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
                saw_report = true;
                case = Some(ModerationCase::opened_by(event));
            }
            ModerationEventKind::PublisherReplied { .. }
            | ModerationEventKind::PublisherAppealed { .. } => {
                let Some(current) = case.as_mut() else {
                    return Err(ModerationWireError::InvalidCase);
                };
                if event.actor_role != ModerationActorRole::Publisher
                    || !event
                        .actor_pubkey_hex
                        .eq_ignore_ascii_case(&current.publisher_pubkey_hex)
                {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
                current.advance(event);
            }
            ModerationEventKind::ReviewerDecided { .. } => {
                let Some(current) = case.as_mut() else {
                    return Err(ModerationWireError::InvalidCase);
                };
                if event.actor_role != ModerationActorRole::Reviewer
                    || !event
                        .actor_pubkey_hex
                        .eq_ignore_ascii_case(owner_pubkey_hex)
                {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
                current.advance(event);
            }
        }
    }
    case.filter(|_| saw_report)
        .ok_or(ModerationWireError::InvalidCase)
}

/// Validate role authorization and immutable case identity for every event in
/// a previously cryptographically verified complete log.
///
/// # Errors
/// Returns the first unauthorized actor or inconsistent case identity.
pub fn verify_moderation_authorization(
    events: &[ModerationEvent],
    owner_pubkey_hex: &str,
) -> Result<(), ModerationWireError> {
    let mut cases: HashMap<&str, (&str, &str)> = HashMap::new();
    for event in events {
        if !event
            .signed_mutation
            .destination
            .as_ref()
            .is_some_and(|destination| {
                destination
                    .owner_pubkey_hex
                    .eq_ignore_ascii_case(owner_pubkey_hex)
            })
        {
            return Err(ModerationWireError::UnauthorizedActor(event.sequence));
        }
        if let Some((package, publisher)) = cases.get(event.case_id.as_str())
            && (*package != event.package_id.as_str()
                || !publisher.eq_ignore_ascii_case(&event.publisher_pubkey_hex))
        {
            return Err(ModerationWireError::InvalidCase);
        }
        match &event.event {
            ModerationEventKind::ReportFiled { .. } => {
                if event.actor_role != ModerationActorRole::Reporter
                    || cases
                        .insert(
                            event.case_id.as_str(),
                            (
                                event.package_id.as_str(),
                                event.publisher_pubkey_hex.as_str(),
                            ),
                        )
                        .is_some()
                {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
            }
            ModerationEventKind::PublisherReplied { .. }
            | ModerationEventKind::PublisherAppealed { .. } => {
                let Some((_, publisher)) = cases.get(event.case_id.as_str()) else {
                    return Err(ModerationWireError::InvalidCase);
                };
                if event.actor_role != ModerationActorRole::Publisher
                    || !event.actor_pubkey_hex.eq_ignore_ascii_case(publisher)
                {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
            }
            ModerationEventKind::ReviewerDecided { .. } => {
                if !cases.contains_key(event.case_id.as_str()) {
                    return Err(ModerationWireError::InvalidCase);
                }
                if event.actor_role != ModerationActorRole::Reviewer
                    || !event
                        .actor_pubkey_hex
                        .eq_ignore_ascii_case(owner_pubkey_hex)
                {
                    return Err(ModerationWireError::UnauthorizedActor(event.sequence));
                }
            }
        }
    }
    Ok(())
}

fn event_matches_mutation(event: &ModerationEvent) -> bool {
    match (&event.event, &event.signed_mutation.mutation) {
        (
            ModerationEventKind::ReportFiled {
                category: event_category,
                summary: event_summary,
                evidence_digest_hex: event_digest,
            },
            ModerationMutation::FileReport {
                package_id,
                category: request_category,
                summary: request_summary,
                evidence_digest_hex: request_digest,
            },
        ) => {
            event.package_id == *package_id
                && event_category == request_category
                && event_summary == request_summary
                && event_digest == request_digest
        }
        (
            ModerationEventKind::PublisherReplied {
                message: event_text,
                evidence_digest_hex: event_digest,
            },
            ModerationMutation::PublisherReply {
                case_id,
                message: request_text,
                evidence_digest_hex: request_digest,
            },
        )
        | (
            ModerationEventKind::PublisherAppealed {
                reason: event_text,
                evidence_digest_hex: event_digest,
            },
            ModerationMutation::PublisherAppeal {
                case_id,
                reason: request_text,
                evidence_digest_hex: request_digest,
            },
        ) => {
            event.case_id == *case_id
                && event_text == request_text
                && event_digest == request_digest
        }
        (
            ModerationEventKind::ReviewerDecided {
                decision: event_decision,
                reason: event_reason,
            },
            ModerationMutation::ReviewerDecision {
                case_id,
                decision: request_decision,
                reason: request_reason,
            },
        ) => {
            event.case_id == *case_id
                && event_decision == request_decision
                && event_reason == request_reason
        }
        _ => false,
    }
}

/// Hash the canonical event with its own digest field cleared.
///
/// # Errors
/// Returns an encoding error if the event cannot be serialized.
pub fn transparency_event_hash(event: &ModerationEvent) -> Result<String, ModerationWireError> {
    let mut unsigned = event.clone();
    unsigned.record_hash_hex.clear();
    let bytes = postcard::to_stdvec(&unsigned).map_err(|_| ModerationWireError::Encoding)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    /// One mutation signed for the test registry whose owner is `owner`.
    fn sign(
        request_id: &str,
        issued_at_unix: u64,
        expires_at_unix: u64,
        mutation: ModerationMutation,
        key: &SigningKey,
        owner: &SigningKey,
    ) -> SignedModerationMutation {
        crate::sign_external_moderation(
            &crate::ExternalModerationOperation {
                destination: crate::ModerationDestination {
                    base_url: "https://registry.test".into(),
                    owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                },
                request_id: request_id.into(),
                issued_at_unix,
                expires_at_unix,
                mutation,
            },
            key,
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn moderation_records_bind_their_signatures_and_chain() {
        // phase: signature_binds_role_expiry_and_body
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut request = sign(
                "request-1",
                10,
                20,
                ModerationMutation::FileReport {
                    package_id: "so.tilo.notes".into(),
                    category: "security".into(),
                    summary: "Unexpected network access".into(),
                    evidence_digest_hex: "aa".repeat(32),
                },
                &key,
                &SigningKey::from_bytes(&[2; 32]),
            );
            verify_moderation_mutation(&request).unwrap();
            request.expires_at_unix = 21;
            assert_eq!(
                verify_moderation_mutation(&request),
                Err(ModerationWireError::InvalidSignature)
            );
        }
        // phase: transparency_chain_detects_edit
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = sign(
                "r",
                1,
                20,
                ModerationMutation::FileReport {
                    package_id: "so.tilo.notes".into(),
                    category: "quality".into(),
                    summary: "broken application".into(),
                    evidence_digest_hex: "bb".repeat(32),
                },
                &key,
                &SigningKey::from_bytes(&[2; 32]),
            );
            let mut event = ModerationEvent {
                sequence: 1,
                case_id: "case".into(),
                package_id: "so.tilo.notes".into(),
                publisher_pubkey_hex: "dd".repeat(32),
                recorded_at_unix: 1,
                actor_role: ModerationActorRole::Reporter,
                actor_pubkey_hex: request.actor_pubkey_hex.clone(),
                request_id: "r".into(),
                event: ModerationEventKind::ReportFiled {
                    category: "quality".into(),
                    summary: "broken application".into(),
                    evidence_digest_hex: "bb".repeat(32),
                },
                actor_signature_hex: request.signature_hex.clone(),
                signed_mutation: request,
                previous_hash_hex: String::new(),
                record_hash_hex: String::new(),
            };
            event.record_hash_hex = transparency_event_hash(&event).unwrap();
            verify_transparency_chain(&[event.clone()], "").unwrap();
            event.package_id.push_str(".evil");
            assert!(verify_transparency_chain(&[event], "").is_err());
        }
        // phase: verified_case_derives_status_and_rejects_forged_reviewer
        {
            let reporter = SigningKey::from_bytes(&[1; 32]);
            let owner = SigningKey::from_bytes(&[2; 32]);
            let outsider = SigningKey::from_bytes(&[3; 32]);
            let report = sign(
                "report",
                10,
                20,
                ModerationMutation::FileReport {
                    package_id: "so.tilo.notes".into(),
                    category: "security".into(),
                    summary: "unexpected network".into(),
                    evidence_digest_hex: "aa".repeat(32),
                },
                &reporter,
                &owner,
            );
            let mut first = ModerationEvent {
                sequence: 1,
                case_id: "case-one".into(),
                package_id: "so.tilo.notes".into(),
                publisher_pubkey_hex: "44".repeat(32),
                recorded_at_unix: 11,
                actor_role: report.actor_role,
                actor_pubkey_hex: report.actor_pubkey_hex.clone(),
                request_id: report.request_id.clone(),
                event: ModerationEventKind::ReportFiled {
                    category: "security".into(),
                    summary: "unexpected network".into(),
                    evidence_digest_hex: "aa".repeat(32),
                },
                actor_signature_hex: report.signature_hex.clone(),
                signed_mutation: report,
                previous_hash_hex: String::new(),
                record_hash_hex: String::new(),
            };
            first.record_hash_hex = transparency_event_hash(&first).unwrap();

            let decision_for = |key: &SigningKey| {
                sign(
                    "decision",
                    12,
                    20,
                    ModerationMutation::ReviewerDecision {
                        case_id: "case-one".into(),
                        decision: ModerationDecision::Quarantine,
                        reason: "confirmed".into(),
                    },
                    key,
                    &owner,
                )
            };
            let make_second = |request: SignedModerationMutation| {
                let mut event = ModerationEvent {
                    sequence: 2,
                    case_id: "case-one".into(),
                    package_id: "so.tilo.notes".into(),
                    publisher_pubkey_hex: "44".repeat(32),
                    recorded_at_unix: 13,
                    actor_role: request.actor_role,
                    actor_pubkey_hex: request.actor_pubkey_hex.clone(),
                    request_id: request.request_id.clone(),
                    event: ModerationEventKind::ReviewerDecided {
                        decision: ModerationDecision::Quarantine,
                        reason: "confirmed".into(),
                    },
                    actor_signature_hex: request.signature_hex.clone(),
                    signed_mutation: request,
                    previous_hash_hex: first.record_hash_hex.clone(),
                    record_hash_hex: String::new(),
                };
                event.record_hash_hex = transparency_event_hash(&event).unwrap();
                event
            };
            let forged = make_second(decision_for(&outsider));
            assert!(matches!(
                derive_verified_case(
                    &[first.clone(), forged.clone()],
                    "case-one",
                    &hex::encode(owner.verifying_key().to_bytes()),
                    &forged.record_hash_hex
                ),
                Err(ModerationWireError::UnauthorizedActor(2))
            ));
            let approved = make_second(decision_for(&owner));
            let case = derive_verified_case(
                &[first, approved.clone()],
                "case-one",
                &hex::encode(owner.verifying_key().to_bytes()),
                &approved.record_hash_hex,
            )
            .unwrap();
            assert_eq!(case.status, ModerationCaseStatus::Quarantined);
            assert_eq!(case.latest_sequence, 2);
        }
    }
}
