//! Authenticated public moderation cases and tamper-evident transparency log.

use std::sync::{Arc, Mutex};

use registry_types::{
    ModerationActorRole, ModerationCase, ModerationCaseStatus, ModerationDecision, ModerationEvent,
    ModerationEventKind, ModerationMutation, SignedModerationMutation, TransparencyPage,
    transparency_event_hash, verify_moderation_mutation,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ModerationService, PublisherAllowlist, ServerIdentity, Storage, StorageError};

const MAX_CLOCK_SKEW_SECONDS: u64 = 300;
const MAX_VALIDITY_SECONDS: u64 = 86_400;
const MAX_PAGE: usize = 100;

#[derive(Clone)]
pub struct ModerationApiService {
    storage: Storage,
    moderation: ModerationService,
    identity: ServerIdentity,
    transaction: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RequestReceipt {
    digest_hex: String,
    case: ModerationCase,
}

impl ModerationApiService {
    pub fn new(storage: Storage, identity: ServerIdentity) -> Self {
        Self {
            moderation: ModerationService::new(storage.clone()),
            storage,
            identity,
            transaction: Arc::new(Mutex::new(())),
        }
    }

    /// Verify, authorize and durably apply one mutation. A byte-identical
    /// retry returns the prior authoritative case; request-id reuse with a
    /// changed signed body is rejected.
    #[allow(
        clippy::too_many_lines,
        reason = "this is the intentionally linear, auditable transaction boundary: validation, replay recovery, authoritative side effect, then three ordered durable renames"
    )]
    pub fn apply(
        &self,
        request: &SignedModerationMutation,
        now_unix: u64,
        allowlist: &PublisherAllowlist,
    ) -> Result<ModerationCase, ModerationApiError> {
        validate_request(request, now_unix)?;
        let operation = request
            .external_operation()
            .map_err(|_| ModerationApiError::BadSignature)?;
        operation
            .validate()
            .map_err(|_| ModerationApiError::Invalid("invalid external operation"))?;
        if operation.destination.base_url != self.identity.base_url
            || !operation
                .destination
                .owner_pubkey_hex
                .eq_ignore_ascii_case(&self.identity.owner_pubkey_hex)
        {
            return Err(ModerationApiError::Forbidden);
        }
        verify_moderation_mutation(request).map_err(|_| ModerationApiError::BadSignature)?;
        let digest_hex = hex::encode(
            registry_types::moderation_signing_message(request)
                .map_err(|_| ModerationApiError::BadSignature)?,
        );
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| ModerationApiError::Unavailable)?;
        if let Some(receipt) = self
            .storage
            .load_moderation_request::<RequestReceipt>(&request.request_id)?
        {
            return if receipt.digest_hex == digest_hex {
                Ok(receipt.case)
            } else {
                Err(ModerationApiError::RequestIdReused)
            };
        }

        let mut events = self.storage.load_transparency::<ModerationEvent>()?;
        events.sort_by_key(|event| event.sequence);
        verify_complete_log(&events)?;
        if let Some(event) = events
            .iter()
            .find(|event| event.request_id == request.request_id)
        {
            let recorded_digest = hex::encode(
                registry_types::moderation_signing_message(&event.signed_mutation)
                    .map_err(|_| ModerationApiError::CorruptLog)?,
            );
            if recorded_digest != digest_hex {
                return Err(ModerationApiError::RequestIdReused);
            }
            let case = rebuild_case(events.iter().filter(|item| item.case_id == event.case_id))
                .ok_or(ModerationApiError::CorruptLog)?;
            self.storage.save_moderation_case(&case.case_id, &case)?;
            self.storage.save_moderation_request(
                &request.request_id,
                &RequestReceipt {
                    digest_hex,
                    case: case.clone(),
                },
            )?;
            return Ok(case);
        }
        if let ModerationMutation::FileReport {
            package_id,
            evidence_digest_hex,
            ..
        } = &request.mutation
            && let Some(existing) = events.iter().rev().find(|event| {
                event.package_id == *package_id
                    && event.actor_pubkey_hex == request.actor_pubkey_hex
                    && matches!(
                        &event.event,
                        ModerationEventKind::ReportFiled {
                            evidence_digest_hex: existing_digest,
                            ..
                        } if existing_digest == evidence_digest_hex
                    )
            })
            && let Some(case) = rebuild_case(
                events
                    .iter()
                    .filter(|event| event.case_id == existing.case_id),
            )
            && !matches!(
                case.status,
                ModerationCaseStatus::ClosedNoAction | ModerationCaseStatus::Restored
            )
        {
            self.storage.save_moderation_request(
                &request.request_id,
                &RequestReceipt {
                    digest_hex,
                    case: case.clone(),
                },
            )?;
            return Ok(case);
        }
        let prior_case_status = match &request.mutation {
            ModerationMutation::ReviewerDecision { case_id, .. } => {
                rebuild_case(events.iter().filter(|event| event.case_id == *case_id))
                    .map(|case| case.status)
            }
            _ => None,
        };
        let (mut case, event_kind) = self.authorize_and_prepare(request, now_unix, allowlist)?;

        if let ModerationMutation::ReviewerDecision {
            decision, reason, ..
        } = &request.mutation
        {
            match decision {
                ModerationDecision::Quarantine => {
                    self.moderation
                        .quarantine(
                            &case.package_id,
                            &request.actor_pubkey_hex,
                            reason,
                            now_unix,
                        )
                        .map_err(|error| ModerationApiError::Moderation(error.to_string()))?;
                }
                ModerationDecision::Restore => {
                    let restored_before_log = prior_case_status
                        == Some(ModerationCaseStatus::Quarantined)
                        && self.storage.load_entry(&case.package_id)?.is_some()
                        && self
                            .storage
                            .load_quarantined_entry(&case.package_id)?
                            .is_none();
                    if !restored_before_log {
                        self.moderation
                            .restore_quarantined(
                                &case.package_id,
                                &request.actor_pubkey_hex,
                                reason,
                                now_unix,
                                allowlist,
                            )
                            .map_err(|error| ModerationApiError::Moderation(error.to_string()))?;
                    }
                }
                ModerationDecision::NeedsInformation | ModerationDecision::NoAction => {}
            }
        }

        let sequence = events
            .last()
            .map_or(1, |event| event.sequence.saturating_add(1));
        let previous_hash_hex = events
            .last()
            .map_or_else(String::new, |event| event.record_hash_hex.clone());
        let mut event = ModerationEvent {
            sequence,
            case_id: case.case_id.clone(),
            package_id: case.package_id.clone(),
            publisher_pubkey_hex: case.publisher_pubkey_hex.clone(),
            recorded_at_unix: now_unix,
            actor_role: request.actor_role,
            actor_pubkey_hex: request.actor_pubkey_hex.clone(),
            request_id: request.request_id.clone(),
            event: event_kind,
            actor_signature_hex: request.signature_hex.clone(),
            signed_mutation: request.clone(),
            previous_hash_hex,
            record_hash_hex: String::new(),
        };
        event.record_hash_hex =
            transparency_event_hash(&event).map_err(|_| ModerationApiError::CorruptLog)?;
        case.updated_at_unix = now_unix;
        case.latest_sequence = sequence;

        // Publish the immutable event before its materialized view. A crash at
        // either following atomic rename is repaired on restart/retry from the
        // authoritative log by `case()`.
        self.storage.save_transparency(sequence, &event)?;
        self.storage.save_moderation_case(&case.case_id, &case)?;
        self.storage.save_moderation_request(
            &request.request_id,
            &RequestReceipt {
                digest_hex,
                case: case.clone(),
            },
        )?;
        Ok(case)
    }

    /// Read and, after a crash, repair a case materialization from the signed log.
    pub fn case(&self, case_id: &str) -> Result<Option<ModerationCase>, ModerationApiError> {
        validate_id(case_id)?;
        let mut events = self.storage.load_transparency::<ModerationEvent>()?;
        events.sort_by_key(|event| event.sequence);
        verify_complete_log(&events)?;
        let rebuilt = rebuild_case(events.iter().filter(|event| event.case_id == case_id));
        let materialized = self
            .storage
            .load_moderation_case::<ModerationCase>(case_id)?;
        if rebuilt != materialized
            && let Some(case) = rebuilt.as_ref()
        {
            self.storage.save_moderation_case(case_id, case)?;
        }
        Ok(rebuilt)
    }

    /// Read one bounded, ordered page of the public transparency log.
    pub fn transparency(
        &self,
        from_sequence: u64,
        limit: usize,
    ) -> Result<TransparencyPage, ModerationApiError> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(ModerationApiError::Invalid("limit must be 1..=100"));
        }
        let mut all = self.storage.load_transparency::<ModerationEvent>()?;
        all.sort_by_key(|event| event.sequence);
        verify_complete_log(&all)?;
        let head = all
            .last()
            .map_or_else(String::new, |event| event.record_hash_hex.clone());
        let maximum_sequence = all.last().map_or(0, |event| event.sequence);
        let events: Vec<_> = all
            .into_iter()
            .filter(|event| event.sequence >= from_sequence.max(1))
            .take(limit)
            .collect();
        let next_sequence = events
            .last()
            .map(|event| event.sequence.saturating_add(1))
            .filter(|next| *next <= maximum_sequence);
        Ok(TransparencyPage {
            events,
            next_sequence,
            chain_head_hex: head,
        })
    }

    fn authorize_and_prepare(
        &self,
        request: &SignedModerationMutation,
        now: u64,
        _allowlist: &PublisherAllowlist,
    ) -> Result<(ModerationCase, ModerationEventKind), ModerationApiError> {
        match &request.mutation {
            ModerationMutation::FileReport {
                package_id,
                category,
                summary,
                evidence_digest_hex,
            } => {
                if request.actor_role != ModerationActorRole::Reporter {
                    return Err(ModerationApiError::Forbidden);
                }
                let entry = self
                    .storage
                    .load_entry(package_id)?
                    .or_else(|| {
                        self.storage
                            .load_quarantined_entry(package_id)
                            .ok()
                            .flatten()
                    })
                    .ok_or(ModerationApiError::NotFound)?;
                Ok((
                    ModerationCase {
                        case_id: registry_types::moderation_case_id(
                            package_id,
                            &request.request_id,
                        ),
                        package_id: package_id.clone(),
                        publisher_pubkey_hex: entry.publisher_pubkey_hex,
                        status: ModerationCaseStatus::Open,
                        opened_at_unix: now,
                        updated_at_unix: now,
                        latest_sequence: 0,
                    },
                    ModerationEventKind::ReportFiled {
                        category: category.clone(),
                        summary: summary.clone(),
                        evidence_digest_hex: evidence_digest_hex.clone(),
                    },
                ))
            }
            ModerationMutation::PublisherReply {
                case_id,
                message,
                evidence_digest_hex,
            } => {
                let case = self.publisher_case(request, case_id)?;
                Ok((
                    case,
                    ModerationEventKind::PublisherReplied {
                        message: message.clone(),
                        evidence_digest_hex: evidence_digest_hex.clone(),
                    },
                ))
            }
            ModerationMutation::PublisherAppeal {
                case_id,
                reason,
                evidence_digest_hex,
            } => {
                let mut case = self.publisher_case(request, case_id)?;
                case.status = ModerationCaseStatus::Appealed;
                Ok((
                    case,
                    ModerationEventKind::PublisherAppealed {
                        reason: reason.clone(),
                        evidence_digest_hex: evidence_digest_hex.clone(),
                    },
                ))
            }
            ModerationMutation::ReviewerDecision {
                case_id,
                decision,
                reason,
            } => {
                if request.actor_role != ModerationActorRole::Reviewer
                    || !request
                        .actor_pubkey_hex
                        .eq_ignore_ascii_case(&self.identity.owner_pubkey_hex)
                {
                    return Err(ModerationApiError::Forbidden);
                }
                let mut case = self.case(case_id)?.ok_or(ModerationApiError::NotFound)?;
                case.status = decision.status();
                Ok((
                    case,
                    ModerationEventKind::ReviewerDecided {
                        decision: *decision,
                        reason: reason.clone(),
                    },
                ))
            }
        }
    }

    fn publisher_case(
        &self,
        request: &SignedModerationMutation,
        case_id: &str,
    ) -> Result<ModerationCase, ModerationApiError> {
        if request.actor_role != ModerationActorRole::Publisher {
            return Err(ModerationApiError::Forbidden);
        }
        let case = self.case(case_id)?.ok_or(ModerationApiError::NotFound)?;
        if !case
            .publisher_pubkey_hex
            .eq_ignore_ascii_case(&request.actor_pubkey_hex)
        {
            return Err(ModerationApiError::Forbidden);
        }
        Ok(case)
    }
}

fn validate_request(
    request: &SignedModerationMutation,
    now: u64,
) -> Result<(), ModerationApiError> {
    if request.protocol != registry_types::MODERATION_PROTOCOL {
        return Err(ModerationApiError::Invalid("unsupported protocol"));
    }
    validate_id(&request.request_id)?;
    if request.expires_at_unix <= request.issued_at_unix
        || request.issued_at_unix > now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
        || now > request.expires_at_unix
        || request
            .expires_at_unix
            .saturating_sub(request.issued_at_unix)
            > MAX_VALIDITY_SECONDS
    {
        return Err(ModerationApiError::Expired);
    }
    if request.actor_pubkey_hex.len() != 64 || request.signature_hex.len() != 128 {
        return Err(ModerationApiError::BadSignature);
    }
    let check_digest =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    match &request.mutation {
        ModerationMutation::FileReport {
            package_id,
            category,
            summary,
            evidence_digest_hex,
        } => {
            validate_package(package_id)?;
            bounded(category, 1, 64)?;
            bounded(summary, 8, 4_096)?;
            if !check_digest(evidence_digest_hex) {
                return Err(ModerationApiError::Invalid("invalid evidence digest"));
            }
        }
        ModerationMutation::PublisherReply {
            case_id,
            message,
            evidence_digest_hex,
        } => {
            validate_id(case_id)?;
            bounded(message, 1, 8_192)?;
            if evidence_digest_hex
                .as_deref()
                .is_some_and(|value| !check_digest(value))
            {
                return Err(ModerationApiError::Invalid("invalid evidence digest"));
            }
        }
        ModerationMutation::PublisherAppeal {
            case_id,
            reason,
            evidence_digest_hex,
        } => {
            validate_id(case_id)?;
            bounded(reason, 8, 8_192)?;
            if evidence_digest_hex
                .as_deref()
                .is_some_and(|value| !check_digest(value))
            {
                return Err(ModerationApiError::Invalid("invalid evidence digest"));
            }
        }
        ModerationMutation::ReviewerDecision {
            case_id, reason, ..
        } => {
            validate_id(case_id)?;
            bounded(reason, 1, 4_096)?;
        }
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), ModerationApiError> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Err(ModerationApiError::Invalid("invalid identifier"))
    } else {
        Ok(())
    }
}
fn validate_package(value: &str) -> Result<(), ModerationApiError> {
    validate_id(value)
}
fn bounded(value: &str, min: usize, max: usize) -> Result<(), ModerationApiError> {
    let count = value.chars().count();
    if count < min || count > max {
        Err(ModerationApiError::Invalid("text outside allowed bounds"))
    } else {
        Ok(())
    }
}

/// A log that is whole from its first event: contiguous, chained and signed.
fn verify_complete_log(events: &[ModerationEvent]) -> Result<(), ModerationApiError> {
    registry_types::verify_transparency_chain(events, "")
        .map(|_| ())
        .map_err(|_| ModerationApiError::CorruptLog)
}

/// The case the log's events for it (in order) leave behind.
fn rebuild_case<'a>(events: impl Iterator<Item = &'a ModerationEvent>) -> Option<ModerationCase> {
    let mut case: Option<ModerationCase> = None;
    for event in events {
        case.get_or_insert_with(|| ModerationCase::opened_by(event))
            .advance(event);
    }
    case
}

#[derive(Debug, Error)]
pub enum ModerationApiError {
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("request expired or has an invalid validity window")]
    Expired,
    #[error("signature verification failed")]
    BadSignature,
    #[error("actor is not authorized for this operation")]
    Forbidden,
    #[error("moderation case or package was not found")]
    NotFound,
    #[error("request id was already used for a different mutation")]
    RequestIdReused,
    #[error("transparency log verification failed")]
    CorruptLog,
    #[error("moderation transition failed: {0}")]
    Moderation(String),
    #[error("moderation service is unavailable")]
    Unavailable,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use registry_types::{
        ExternalModerationOperation, ModerationDestination, RegistryEntry, RegistryTarget,
        RegistryVersion, TILO_OS_PLATFORM, sign_entry, sign_external_moderation,
    };

    fn setup() -> (
        tempfile::TempDir,
        ModerationApiService,
        SigningKey,
        SigningKey,
        PublisherAllowlist,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(directory.path()).unwrap();
        let publisher = SigningKey::generate(&mut UnwrapErr(SysRng));
        let reviewer = SigningKey::from_bytes(&[42; 32]);
        let mut entry = RegistryEntry {
            package_id: "so.tilo.notes".into(),
            kind: registry_types::EntryKind::App,
            name: "Notes".into(),
            description: "Native notes app".into(),
            tagline: "Write together".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec![],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: "https://apps.tilo.so/artifacts/notes/notes.tapp".into(),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 1,
                    tapp_blake3_hex: "cd".repeat(32),
                    target: RegistryTarget {
                        platform: TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &publisher);
        storage.save_entry(&entry).unwrap();
        let mut allowlist = PublisherAllowlist::empty();
        allowlist.upsert(crate::PublisherRow {
            pubkey_hex: entry.publisher_pubkey_hex,
            can_add_new: true,
            note: String::new(),
            namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
        });
        let identity = ServerIdentity {
            owner_pubkey_hex: hex::encode(reviewer.verifying_key().to_bytes()),
            name: "Test".into(),
            description: String::new(),
            base_url: "https://registry.test".into(),
        };
        let service = ModerationApiService::new(storage, identity);
        (directory, service, publisher, reviewer, allowlist)
    }

    fn sign_moderation_mutation(
        request_id: String,
        issued_at_unix: u64,
        expires_at_unix: u64,
        role: ModerationActorRole,
        mutation: ModerationMutation,
        key: &SigningKey,
    ) -> Result<SignedModerationMutation, registry_types::ModerationWireError> {
        let request = sign_external_moderation(
            &ExternalModerationOperation {
                destination: ModerationDestination {
                    base_url: "https://registry.test".into(),
                    owner_pubkey_hex: hex::encode(
                        SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes(),
                    ),
                },
                request_id,
                issued_at_unix,
                expires_at_unix,
                mutation,
            },
            key,
        )?;
        if request.actor_role != role {
            return Err(registry_types::ModerationWireError::InvalidOperation(
                format!("signed as {:?}, expected {role:?}", request.actor_role),
            ));
        }
        Ok(request)
    }

    fn report(key: &SigningKey, request_id: &str) -> SignedModerationMutation {
        sign_moderation_mutation(
            request_id.into(),
            100,
            200,
            ModerationActorRole::Reporter,
            ModerationMutation::FileReport {
                package_id: "so.tilo.notes".into(),
                category: "security".into(),
                summary: "Unexpected network connection".into(),
                evidence_digest_hex: "aa".repeat(32),
            },
            key,
        )
        .unwrap()
    }

    #[test]
    fn moderation_rejects_other_protocols_and_destinations_before_writing() {
        let (_directory, service, _publisher, _reviewer, allowlist) = setup();
        let actor = SigningKey::from_bytes(&[7; 32]);
        let request = report(&actor, "destination-test");
        let mut other_protocol = request.clone();
        other_protocol.protocol = registry_types::MODERATION_PROTOCOL + 1;
        assert!(matches!(
            service.apply(&other_protocol, 110, &allowlist),
            Err(ModerationApiError::Invalid("unsupported protocol"))
        ));
        for change_owner in [false, true] {
            let mut operation = request.external_operation().unwrap();
            if change_owner {
                operation.destination.owner_pubkey_hex = "ab".repeat(32);
            } else {
                operation.destination.base_url.push('/');
            }
            let foreign = sign_external_moderation(&operation, &actor).unwrap();
            registry_types::verify_moderation_mutation(&foreign).unwrap();
            assert!(matches!(
                service.apply(&foreign, 110, &allowlist),
                Err(ModerationApiError::Forbidden)
            ));
        }
        assert!(service.transparency(1, 10).unwrap().events.is_empty());
        service.apply(&request, 110, &allowlist).unwrap();
        assert_eq!(service.transparency(1, 10).unwrap().events.len(), 1);
    }

    #[test]
    fn moderation_requests_are_signed_idempotent_and_fresh() {
        // phase: report_is_signed_idempotent_and_publicly_verifiable
        {
            let (directory, service, _publisher, _reviewer, allowlist) = setup();
            let reporter = SigningKey::generate(&mut UnwrapErr(SysRng));
            let request = report(&reporter, "request-one");
            let first = service.apply(&request, 110, &allowlist).unwrap();
            assert_eq!(service.apply(&request, 111, &allowlist).unwrap(), first);
            let page = service.transparency(1, 10).unwrap();
            assert_eq!(page.events.len(), 1);
            registry_types::verify_transparency_chain(&page.events, "").unwrap();
            assert_eq!(service.case(&first.case_id).unwrap(), Some(first.clone()));

            // Simulate a crash after the immutable event rename but before the
            // materialized case/receipt renames. Retry repairs both without a
            // second public event.
            std::fs::remove_file(
                directory
                    .path()
                    .join("moderation-requests/request-one.json"),
            )
            .unwrap();
            std::fs::remove_file(
                directory
                    .path()
                    .join(format!("moderation-cases/{}.json", first.case_id)),
            )
            .unwrap();
            service.apply(&request, 112, &allowlist).unwrap();
            assert_eq!(service.transparency(1, 10).unwrap().events.len(), 1);
            let clicked_retry = report(&reporter, "new-id-same-evidence");
            assert_eq!(
                service.apply(&clicked_retry, 113, &allowlist).unwrap(),
                first
            );
            assert_eq!(service.transparency(1, 10).unwrap().events.len(), 1);
        }
        // phase: request_id_reuse_and_expiry_are_rejected
        {
            let (_directory, service, _publisher, _reviewer, allowlist) = setup();
            let reporter = SigningKey::generate(&mut UnwrapErr(SysRng));
            service
                .apply(&report(&reporter, "same"), 110, &allowlist)
                .unwrap();
            let changed = sign_moderation_mutation(
                "same".into(),
                100,
                200,
                ModerationActorRole::Reporter,
                ModerationMutation::FileReport {
                    package_id: "so.tilo.notes".into(),
                    category: "quality".into(),
                    summary: "A different signed report".into(),
                    evidence_digest_hex: "bb".repeat(32),
                },
                &reporter,
            )
            .unwrap();
            assert!(matches!(
                service.apply(&changed, 110, &allowlist),
                Err(ModerationApiError::RequestIdReused)
            ));
            assert!(matches!(
                service.apply(&report(&reporter, "expired"), 201, &allowlist),
                Err(ModerationApiError::Expired)
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn moderation_decisions_are_role_authorised_and_recoverable() {
        // phase: only_package_publisher_can_reply_or_appeal
        {
            let (_directory, service, publisher, _reviewer, allowlist) = setup();
            let reporter = SigningKey::generate(&mut UnwrapErr(SysRng));
            let case = service
                .apply(&report(&reporter, "file"), 110, &allowlist)
                .unwrap();
            let attacker = SigningKey::generate(&mut UnwrapErr(SysRng));
            let make = |key: &SigningKey, id: &str| {
                sign_moderation_mutation(
                    id.into(),
                    111,
                    200,
                    ModerationActorRole::Publisher,
                    ModerationMutation::PublisherAppeal {
                        case_id: case.case_id.clone(),
                        reason: "The report has omitted important context".into(),
                        evidence_digest_hex: None,
                    },
                    key,
                )
                .unwrap()
            };
            assert!(matches!(
                service.apply(&make(&attacker, "bad"), 112, &allowlist),
                Err(ModerationApiError::Forbidden)
            ));
            let appealed = service
                .apply(&make(&publisher, "good"), 112, &allowlist)
                .unwrap();
            assert_eq!(appealed.status, ModerationCaseStatus::Appealed);
        }
        // phase: only_registry_owner_can_make_reviewer_decision
        {
            let (_directory, service, _publisher, reviewer, allowlist) = setup();
            let reporter = SigningKey::generate(&mut UnwrapErr(SysRng));
            let case = service
                .apply(&report(&reporter, "file"), 110, &allowlist)
                .unwrap();
            let decision = |key: &SigningKey, id: &str| {
                sign_moderation_mutation(
                    id.into(),
                    111,
                    200,
                    ModerationActorRole::Reviewer,
                    ModerationMutation::ReviewerDecision {
                        case_id: case.case_id.clone(),
                        decision: ModerationDecision::NoAction,
                        reason: "The signed evidence does not demonstrate a policy violation"
                            .into(),
                    },
                    key,
                )
                .unwrap()
            };
            assert!(matches!(
                service.apply(&decision(&reporter, "bad-decision"), 112, &allowlist),
                Err(ModerationApiError::Forbidden)
            ));
            assert_eq!(
                service
                    .apply(&decision(&reviewer, "good-decision"), 112, &allowlist)
                    .unwrap()
                    .status,
                ModerationCaseStatus::ClosedNoAction
            );
        }
        // phase: restore_recovers_when_catalogue_effect_was_durable_before_public_event
        {
            let (_directory, service, _publisher, reviewer, allowlist) = setup();
            let reporter = SigningKey::generate(&mut UnwrapErr(SysRng));
            let case = service
                .apply(&report(&reporter, "file-for-recovery"), 110, &allowlist)
                .unwrap();
            let quarantine = sign_moderation_mutation(
                "quarantine-first".into(),
                111,
                200,
                ModerationActorRole::Reviewer,
                ModerationMutation::ReviewerDecision {
                    case_id: case.case_id.clone(),
                    decision: ModerationDecision::Quarantine,
                    reason: "Hold while the signed evidence is investigated".into(),
                },
                &reviewer,
            )
            .unwrap();
            service.apply(&quarantine, 112, &allowlist).unwrap();

            // The catalogue half of restore lands, then the process dies before
            // the API can append its public event.
            service
                .moderation
                .restore_quarantined("so.tilo.notes", "owner", "fixed", 113, &allowlist)
                .unwrap();
            let restore = sign_moderation_mutation(
                "restore-after-crash".into(),
                113,
                200,
                ModerationActorRole::Reviewer,
                ModerationMutation::ReviewerDecision {
                    case_id: case.case_id,
                    decision: ModerationDecision::Restore,
                    reason: "The publisher supplied a verified fixed package".into(),
                },
                &reviewer,
            )
            .unwrap();
            assert_eq!(
                service.apply(&restore, 114, &allowlist).unwrap().status,
                ModerationCaseStatus::Restored
            );
            assert_eq!(service.transparency(1, 10).unwrap().events.len(), 3);
        }
    }
}
