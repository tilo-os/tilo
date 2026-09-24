use qos_peer_state::{NeighbourMsg, StatePeer};
use web_time::SystemTime;

pub(crate) const OUTGOING_CHECKPOINT_MESSAGE: &str =
    "Saved changes are waiting for the session peer to confirm them.";
const MAX_SAFE_EPOCH_MS: u64 = 9_007_199_254_740_991;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutgoingProjection {
    pub(crate) pending: usize,
    pub(crate) blocked_expired: usize,
    pub(crate) expiry_at_ms: Option<u64>,
}

pub(crate) fn outgoing_projection(
    state: &StatePeer,
    outbox: &[NeighbourMsg],
    now: SystemTime,
) -> OutgoingProjection {
    let status = state.outbox_status(outbox, now);
    let expiry_at_ms = outbox
        .iter()
        .filter(|message| {
            !state.outbox_message_retired(message) && qos_peer_state::outbox::fresh(message, now)
        })
        .filter_map(|message| {
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                return None;
            };
            envelope
                .timestamp
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|timestamp| u64::try_from(timestamp.as_millis()).ok())
                .map(|timestamp| timestamp.saturating_add(300_001))
                .filter(|timestamp| *timestamp <= MAX_SAFE_EPOCH_MS)
        })
        .min();
    OutgoingProjection {
        pending: status.pending,
        blocked_expired: status.blocked_expired,
        expiry_at_ms,
    }
}

const MISSING_GOVERNED_DEVICE: &str = "device is absent from governed roster";

/// A fresh enrolled viewer may receive the pre-enrollment checkpoint before its
/// membership checkpoint in the same authenticated replay. Absence is not yet a
/// denial until that replay ends. This never grants permissions, and a hydrated
/// governed owner or a viewer whose membership was observed gets no exemption.
#[derive(Clone)]
pub(crate) struct InitialAdmissionReplay {
    pending: bool,
}
impl InitialAdmissionReplay {
    pub(crate) fn new(state: &StatePeer) -> Self {
        Self {
            pending: !state.has_governance(),
        }
    }
    pub(crate) fn finish(&mut self) {
        self.pending = false;
    }
    pub(crate) fn waiting(&mut self, state: &StatePeer, device: [u8; 32], now: SystemTime) -> bool {
        if self.pending {
            match state.permissions(device, now) {
                Err(error)
                    if error == "room governance missing" || error == MISSING_GOVERNED_DEVICE =>
                {
                    return true;
                }
                _ => self.finish(),
            }
        }
        false
    }
}

/// Only roster absence has this initial replay prerequisite. Explicit room or
/// record denial, invalid certificates, revocations and installation failures
/// must retain their usual terminal meaning.
pub(crate) fn permission_failure(
    failure: Option<String>,
    awaiting_governance: bool,
) -> Option<String> {
    failure.filter(|failure| !(awaiting_governance && failure == MISSING_GOVERNED_DEVICE))
}

pub(crate) fn runtime_phase(
    requested: &str,
    complete: bool,
    persistence_needed: bool,
    was_ready: bool,
    admission_incomplete: bool,
    awaiting_governance: bool,
) -> &str {
    if admission_incomplete && !matches!(requested, "closed" | "failed") {
        "incomplete"
    } else if awaiting_governance && matches!(requested, "admitting" | "ready") {
        "admitting"
    } else if requested == "admitting" && complete && (!persistence_needed || was_ready) {
        "ready"
    } else {
        requested
    }
}

pub(crate) fn durable_admission_failure(
    state: &StatePeer,
    device: [u8; 32],
    now: SystemTime,
    admission_failure: Option<String>,
    awaiting_governance: bool,
) -> Option<String> {
    state.incomplete_reason().or(admission_failure).or_else(|| {
        permission_failure(
            state
                .has_governance()
                .then(|| state.permissions(device, now).err())
                .flatten(),
            awaiting_governance,
        )
    })
}

pub(crate) fn admission_phase(
    state: &StatePeer,
    lifecycle_ready: bool,
    durable_failure: Option<&str>,
) -> &'static str {
    if durable_failure.is_some() {
        "failed"
    } else if lifecycle_ready && state.has_governance() {
        "ready"
    } else {
        "syncing"
    }
}

// Source is carried separately from error text: a receive rejection did not
// install the candidate and cannot describe the durable owner's identity.
#[derive(Clone, Copy)]
pub(crate) enum ReportSource {
    Runtime,
    Transport,
    ReceiveRejection,
    StaleReceive,
}

pub(crate) fn reported_failure<'a>(
    durable: Option<&'a str>,
    reported: Option<&'a str>,
    source: ReportSource,
    usable: bool,
) -> Option<&'a str> {
    durable.or_else(|| {
        if matches!(source, ReportSource::StaleReceive) && usable {
            None
        } else {
            reported
        }
    })
}

pub(crate) fn failure_policy(
    text: &str,
    source: ReportSource,
    durable: bool,
    connected: bool,
) -> (&'static str, &'static str, &'static str) {
    if !durable && matches!(source, ReportSource::StaleReceive) {
        return (
            "prerequisite-missing",
            "Shared state is still catching up.",
            "refresh-checkpoint",
        );
    }
    if text.contains("revoked") {
        (
            "identity-revoked",
            "This device no longer has access.",
            "review-permissions",
        )
    } else if text.contains("MLS") || text.contains("crypto") {
        (
            "crypto-unavailable",
            "Encrypted content is waiting for its security prerequisites.",
            "refresh-checkpoint",
        )
    } else if text.contains("cert") && text.contains("expired") {
        (
            "certificate-expired",
            "This device needs a current authorization certificate.",
            "reauthenticate",
        )
    } else if text.contains("signature") || text.contains("binding") {
        (
            "signature-rejected",
            "Shared state authorization was rejected.",
            "reauthenticate",
        )
    } else if text.contains("forbidden")
        || text.contains("permission")
        || text.contains("governed roster")
    {
        (
            "permission-required",
            "Access to this record needs permission.",
            "review-permissions",
        )
    } else if !connected {
        (
            "transport-ended",
            "The connection was interrupted. Saved work is retained.",
            "retry-connection",
        )
    } else {
        (
            "record-install-failed",
            "Shared state could not be loaded yet.",
            "refresh-checkpoint",
        )
    }
}

pub(crate) fn receive_source(message: &str) -> ReportSource {
    // These are concrete receive validation errors, not arbitrary runtime text.
    match message {
        "op timestamp is outside attestation validity window" | "device cert has expired" => {
            ReportSource::StaleReceive
        }
        _ => ReportSource::ReceiveRejection,
    }
}

#[derive(Default)]
pub(crate) struct AdmissionProgress {
    revision: u64,
    accepted: u64,
    staged: Option<(u64, u64)>,
    durable: u64,
}
impl AdmissionProgress {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn accepted(&mut self, journal_revision: Option<u64>) {
        self.revision += 1;
        if let Some(revision) = journal_revision {
            self.accepted += 1;
            self.staged = Some((revision, self.accepted));
        }
    }
    // Capture the accepted basis *when the exact Rust-owned batch is created*.
    // A later accepted packet cannot change what an in-flight commit covers.
    pub(crate) fn batch_basis(&self, journal_revision: Option<u64>) -> Option<u64> {
        let (staged, basis) = self.staged?;
        (journal_revision? >= staged).then_some(basis)
    }
    pub(crate) fn committed(&mut self, basis: Option<u64>) {
        if let Some(basis) = basis.filter(|basis| *basis > self.durable) {
            self.durable = basis;
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use ed25519_dalek::SigningKey;
    use qos_desktop::projection::RoomProjection;
    use qos_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, sign_device_cert,
    };
    use web_time::{Duration, SystemTime};

    #[test]
    fn receive_context_cannot_reclassify_a_durable_authorization_or_storage_failure() {
        use super::{ReportSource::StaleReceive, failure_policy};
        assert_eq!(
            failure_policy(
                "device is absent from governed roster",
                StaleReceive,
                true,
                true
            )
            .0,
            "permission-required"
        );
        assert_eq!(
            failure_policy("record installation failed", StaleReceive, true, true).0,
            "record-install-failed"
        );
        assert_eq!(
            failure_policy("device cert has expired", StaleReceive, true, true).0,
            "certificate-expired"
        );
        assert_eq!(
            failure_policy("device cert has expired", StaleReceive, false, true).0,
            "prerequisite-missing"
        );
        assert_eq!(
            failure_policy("signature rejected", StaleReceive, true, true).0,
            "signature-rejected"
        );
        assert_eq!(
            failure_policy(
                "op timestamp is outside attestation validity window",
                StaleReceive,
                false,
                true
            )
            .0,
            "prerequisite-missing"
        );
    }

    #[test]
    fn progress_requires_accepted_state_or_its_exact_durable_batch() {
        let mut progress = super::AdmissionProgress::default();
        progress.accepted(Some(7));
        assert_eq!(progress.revision(), 1);
        let first = progress.batch_basis(Some(7));
        progress.accepted(Some(9));
        assert_eq!(progress.revision(), 2);
        progress.committed(first);
        assert_eq!(
            progress.revision(),
            3,
            "older accepted basis is still useful durable progress"
        );
        progress.committed(first);
        progress.committed(None);
        assert_eq!(
            progress.revision(),
            3,
            "duplicate or unrelated acknowledgements are not progress"
        );
        assert_eq!(progress.batch_basis(Some(8)), None);
        progress.committed(progress.batch_basis(Some(9)));
        assert_eq!(progress.revision(), 4);
        progress.accepted(None);
        assert_eq!(
            progress.revision(),
            5,
            "prerequisite resolution without a journal write is accepted progress"
        );
        progress.committed(progress.batch_basis(Some(10)));
        assert_eq!(
            progress.revision(),
            5,
            "local writes and forced publication cannot recommit an accepted basis"
        );
    }

    #[test]
    fn stale_receive_preserves_usable_admission_and_real_failures_take_precedence() {
        use super::{ReportSource::*, receive_source, reported_failure};
        assert!(matches!(
            receive_source("op timestamp is outside attestation validity window"),
            StaleReceive
        ));
        assert!(matches!(
            receive_source("device cert has expired"),
            StaleReceive
        ));
        assert!(matches!(
            receive_source("signature rejected: timestamp"),
            ReceiveRejection
        ));
        assert_eq!(
            reported_failure(
                None,
                Some("op timestamp is outside attestation validity window"),
                StaleReceive,
                true
            ),
            None
        );
        assert_eq!(
            reported_failure(
                Some("device cert has expired"),
                Some("old packet"),
                StaleReceive,
                true
            ),
            Some("device cert has expired")
        );
        assert_eq!(
            reported_failure(
                Some("device is absent from governed roster"),
                Some("old packet"),
                StaleReceive,
                true
            ),
            Some("device is absent from governed roster")
        );
        assert_eq!(
            reported_failure(
                Some("persisted MLS state incomplete"),
                Some("old packet"),
                StaleReceive,
                true
            ),
            Some("persisted MLS state incomplete")
        );
        assert_eq!(
            reported_failure(None, Some("signature rejected"), ReceiveRejection, true),
            Some("signature rejected")
        );
        assert_eq!(
            reported_failure(None, Some("device cert has expired"), Runtime, true),
            Some("device cert has expired")
        );
        assert_eq!(
            reported_failure(None, Some("connection ended"), Transport, true),
            Some("connection ended")
        );
    }

    fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn document() -> SeedDocument {
        let mut document = AutoCommit::new();
        document.put(ROOT, "value", "persisted").unwrap();
        document.commit();
        SeedDocument {
            app_id: "so.tilo.word-processor".into(),
            record_id: "tab:0000000000000001".into(),
            kind: "word-processor".into(),
            bytes: document.save(),
        }
    }

    #[derive(Serialize, Deserialize)]
    struct DurableFixture {
        version: u32,
        room: String,
        authority: DeviceCert,
        room_bytes: Option<Vec<u8>>,
        documents: Vec<DurableDocumentFixture>,
        receipts: BTreeMap<String, Vec<u8>>,
        certificates: Vec<DeviceCert>,
        revocations: Vec<RevocationEntry>,
    }

    /// The current durable envelope: a frozen v3 base plus the pending queue.
    #[derive(Serialize, Deserialize)]
    struct DurableV4Fixture {
        version: u32,
        base: DurableFixture,
        pending: std::collections::VecDeque<([u8; 32], qos_peer_state::NeighbourMsg, bool)>,
        pending_bytes: usize,
    }

    #[derive(Serialize, Deserialize)]
    struct DurableDocumentFixture {
        route: RouteFixture,
        bytes: Vec<u8>,
        watermarks: Vec<([u8; 32], u64)>,
        wire_version: Option<DocumentWireVersionFixture>,
        encryption_required: bool,
        covered: BTreeMap<String, u64>,
        covered_snapshot: Option<Vec<u8>>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum DocumentWireVersionFixture {
        QosV1,
        QosV2,
        TiloV1,
        TiloV2,
    }

    #[derive(Serialize, Deserialize)]
    struct RouteFixture {
        app: String,
        record: String,
    }

    #[test]
    fn initial_replay_preserves_explicit_permission_and_identity_failures() {
        for failure in [
            "person has no room membership",
            "document read forbidden",
            "governed certificate mismatch",
            "device cert has expired",
            "device revoked",
            "signature rejected",
        ] {
            assert_eq!(
                super::permission_failure(Some(failure.into()), true),
                Some(failure.into())
            );
        }
        assert_eq!(
            super::runtime_phase("admitting", true, false, false, true, true),
            "incomplete"
        );
        assert_eq!(
            super::runtime_phase("failed", true, false, false, false, true),
            "failed"
        );
    }

    #[test]
    fn fresh_empty_workspace_waits_for_enrollment_governance_within_initial_replay() {
        use qos_peer_state::{NeighbourMsg, TransportOrigin};
        let root = SigningKey::from_bytes(&[31; 32]);
        let authority_key = SigningKey::from_bytes(&[32; 32]);
        let member_key = SigningKey::from_bytes(&[33; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "new viewer");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let mut native = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        native
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone()]).unwrap(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        let mut empty = native.admitted_room().unwrap().clone();
        RoomProjection::project_owned_contents(&mut empty, &[], 5, None);
        native
            .install_seed(
                SeedState {
                    room_changes: empty
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        let old = native
            .durable_bootstrap_messages(&authority_key, now, 1)
            .unwrap();
        native
            .admit_certificate(member.clone(), &authority_key, now)
            .unwrap();
        let enrolled = native
            .durable_bootstrap_messages(&authority_key, now, 2)
            .unwrap();
        let mut browser = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        let mut replay = super::InitialAdmissionReplay::new(&browser);
        for message in old {
            browser
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(authority.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        assert!(crate::desktop::admission(&browser).unwrap().complete());
        assert!(browser.rows().is_empty());
        assert_eq!(
            browser
                .permissions(member.device_public_key, now)
                .unwrap_err(),
            "device is absent from governed roster"
        );
        let waiting = replay.waiting(&browser, member.device_public_key, now);
        assert!(waiting);
        assert_eq!(
            super::durable_admission_failure(
                &browser,
                member.device_public_key,
                now,
                None,
                waiting
            ),
            None
        );
        assert_eq!(
            super::runtime_phase("admitting", true, false, false, false, waiting),
            "admitting"
        );
        assert_eq!(
            super::runtime_phase("ready", true, false, false, false, waiting),
            "admitting"
        );
        assert_eq!(super::admission_phase(&browser, false, None), "syncing");
        assert_eq!(
            super::durable_admission_failure(
                &browser,
                member.device_public_key,
                now,
                Some("record installation failed".into()),
                waiting
            ),
            Some("record installation failed".into())
        );

        // Reaching Ready without the promised membership exposes the real denial.
        let mut finished = replay.clone();
        finished.finish();
        assert!(!finished.waiting(&browser, member.device_public_key, now));
        assert!(
            super::durable_admission_failure(&browser, member.device_public_key, now, None, false)
                .is_some()
        );
        // A hydrated governed owner never receives an initial replay exemption.
        assert!(!super::InitialAdmissionReplay::new(&browser).waiting(
            &browser,
            member.device_public_key,
            now
        ));

        let missing_membership = browser.clone();
        let mut initially_revoked = browser.clone();
        let mut revoked_replay = replay.clone();
        let mut revocation = RevocationEntry {
            user_public_key: member.user_public_key,
            revoked_device_public_key: member.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
        initially_revoked
            .receive_from(
                TransportOrigin::UntrustedPresence(authority.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(authority.clone()),
                    revocations: vec![revocation.clone()],
                },
                now,
            )
            .unwrap();
        assert!(!revoked_replay.waiting(&initially_revoked, member.device_public_key, now));
        assert!(
            super::durable_admission_failure(
                &initially_revoked,
                member.device_public_key,
                now,
                None,
                true
            )
            .unwrap()
            .contains("revoked")
        );

        for message in enrolled {
            browser
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(authority.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        assert!(browser.permissions(member.device_public_key, now).is_ok());
        assert!(!replay.waiting(&browser, member.device_public_key, now));
        assert!(
            !replay.waiting(&missing_membership, member.device_public_key, now),
            "observed membership permanently ends the initial replay exemption"
        );
        assert_eq!(
            super::durable_admission_failure(&browser, member.device_public_key, now, None, false),
            None
        );
        assert_eq!(super::admission_phase(&browser, true, None), "ready");

        browser
            .receive_from(
                TransportOrigin::UntrustedPresence(authority.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(authority),
                    revocations: vec![revocation],
                },
                now,
            )
            .unwrap();
        assert!(!replay.waiting(&browser, member.device_public_key, now));
        assert!(
            super::durable_admission_failure(&browser, member.device_public_key, now, None, true)
                .unwrap()
                .contains("revoked")
        );
    }

    #[test]
    fn permission_health_waits_for_governance_but_preserves_real_denial() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let authority_key = SigningKey::from_bytes(&[2; 32]);
        let member_key = SigningKey::from_bytes(&[3; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "member");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

        let mut state = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();

        assert!(!state.has_governance());
        assert!(state.permissions(member.device_public_key, now).is_err());
        assert_eq!(
            super::durable_admission_failure(&state, member.device_public_key, now, None, false),
            None,
            "missing governance is a bootstrap prerequisite, not a permission failure"
        );
        assert_eq!(
            super::durable_admission_failure(
                &state,
                member.device_public_key,
                now,
                Some("persisted admission failed".into()),
                false,
            ),
            Some("persisted admission failed".into()),
            "an actual admission failure remains terminal before governance arrives"
        );

        state
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone(), member.clone()])
                        .unwrap(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert!(state.has_governance());
        assert_eq!(
            super::durable_admission_failure(&state, member.device_public_key, now, None, false),
            None,
            "an admitted certificate remains healthy"
        );

        let mut denied = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        denied
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone()]).unwrap(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert_eq!(
            super::durable_admission_failure(&denied, member.device_public_key, now, None, false),
            Some("device is absent from governed roster".into()),
            "governance denial must remain terminal"
        );
    }

    #[test]
    fn admission_phase_requires_governance_and_prioritizes_real_denial() {
        let root = SigningKey::from_bytes(&[11; 32]);
        let authority_key = SigningKey::from_bytes(&[12; 32]);
        let member_key = SigningKey::from_bytes(&[13; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "member");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

        let bootstrap = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(super::admission_phase(&bootstrap, false, None), "syncing");

        let mut admitted = bootstrap.clone();
        admitted
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone(), member.clone()])
                        .unwrap(),
                    documents: vec![document()],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert!(!admitted.rows().is_empty());
        assert_eq!(super::admission_phase(&admitted, true, None), "ready");

        let mut envelope: DurableV4Fixture =
            postcard::from_bytes(&admitted.durable().unwrap()).unwrap();
        assert_eq!(envelope.version, 4);
        let durable = &mut envelope.base;
        assert_eq!(durable.version, 3);
        assert!(
            durable
                .documents
                .iter()
                .all(|document| document.wire_version == Some(DocumentWireVersionFixture::TiloV2))
        );
        durable.room_bytes = None;
        let restored = StatePeer::restore(
            &postcard::to_allocvec(&envelope).unwrap(),
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(!restored.rows().is_empty());
        assert!(!restored.has_governance());
        assert_eq!(super::admission_phase(&restored, true, None), "syncing");

        let mut denied = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        denied
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone()]).unwrap(),
                    documents: vec![document()],
                },
                &authority_key,
                now,
            )
            .unwrap();
        let failure =
            super::durable_admission_failure(&denied, member.device_public_key, now, None, false);
        assert!(!denied.rows().is_empty());
        assert_eq!(
            super::admission_phase(&denied, true, failure.as_deref()),
            "failed"
        );
    }

    #[test]
    fn expired_outbox_for_nonrequired_record_is_checkpoint_notice_not_admission_failure() {
        assert_eq!(
            super::OUTGOING_CHECKPOINT_MESSAGE,
            "Saved changes are waiting for the session peer to confirm them."
        );
        let root = SigningKey::from_bytes(&[21; 32]);
        let authority_key = SigningKey::from_bytes(&[22; 32]);
        let member_key = SigningKey::from_bytes(&[23; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "member");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let route = document();
        let mut client = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        client
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone(), member.clone()])
                        .unwrap(),
                    documents: vec![route.clone()],
                },
                &authority_key,
                now,
            )
            .unwrap();
        let mut desktop = client.admitted_room().unwrap().clone();
        RoomProjection::project_owned_contents(&mut desktop, &[], 5, None);
        client
            .install_seed(
                SeedState {
                    room_changes: desktop
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert!(
            crate::desktop::admission(&client)
                .unwrap()
                .required
                .iter()
                .all(|required| required.app_id != route.app_id
                    || required.record_id != route.record_id),
            "the retained record is closed/non-required"
        );

        let mut governor = client.clone();
        let mut future_client = client.clone();
        let mut future_edit = AutoCommit::load(
            &future_client
                .document(&route.app_id, &route.record_id)
                .unwrap(),
        )
        .unwrap();
        future_edit.set_actor(automerge::ActorId::from(
            member_key.verifying_key().to_bytes().to_vec(),
        ));
        let future_before = future_edit.get_heads();
        future_edit.put(ROOT, "future", "clock-drift").unwrap();
        future_edit.commit();
        let future_time = now + Duration::from_secs(299);
        let future = future_client
            .publish_incremental(
                &route.app_id,
                &route.record_id,
                future_edit.save_after(&future_before),
                &member_key,
                future_time,
            )
            .unwrap();
        let future = super::outgoing_projection(&future_client, &[future], now);
        assert_eq!(future.pending, 1);
        assert_eq!(future.blocked_expired, 0);
        assert_eq!(future.expiry_at_ms, Some(2_599_001));

        let mut edited =
            AutoCommit::load(&client.document(&route.app_id, &route.record_id).unwrap()).unwrap();
        edited.set_actor(automerge::ActorId::from(
            member_key.verifying_key().to_bytes().to_vec(),
        ));
        let before = edited.get_heads();
        edited.put(ROOT, "later", "durable").unwrap();
        edited.commit();
        let original = client
            .publish_incremental(
                &route.app_id,
                &route.record_id,
                edited.save_after(&before),
                &member_key,
                now,
            )
            .unwrap();
        let mut covering_governor = governor.clone();
        covering_governor
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(member_key.verifying_key().to_bytes()),
                original.clone(),
                now,
            )
            .unwrap();
        let mut covered_state = client.clone();
        for message in covering_governor
            .bootstrap_messages(&authority_key, now)
            .unwrap()
        {
            covered_state
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        authority_key.verifying_key().to_bytes(),
                    ),
                    message,
                    now,
                )
                .unwrap();
        }
        assert!(covered_state.covers(&original));
        assert!(
            covered_state
                .durable_coverage(&route.app_id, &route.record_id)
                .unwrap()
                .is_none(),
            "causal coverage alone is not a durable acknowledgement"
        );
        assert!(!covered_state.outbox_message_retired(&original));
        let covered =
            super::outgoing_projection(&covered_state, std::slice::from_ref(&original), now);
        assert_eq!(covered.pending, 1);
        assert_eq!(covered.blocked_expired, 0);
        assert_eq!(
            covered.expiry_at_ms,
            Some(2_300_001),
            "causally covered evidence still needs a bounded durable-checkpoint wakeup"
        );
        let later = now + Duration::from_secs(301);
        let covered_expired =
            super::outgoing_projection(&covered_state, std::slice::from_ref(&original), later);
        assert_eq!(covered_expired.pending, 1);
        assert_eq!(covered_expired.blocked_expired, 1);
        assert_eq!(covered_expired.expiry_at_ms, None);

        for message in covering_governor
            .durable_bootstrap_messages(&authority_key, later, 6)
            .unwrap()
        {
            covered_state
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        authority_key.verifying_key().to_bytes(),
                    ),
                    message,
                    later,
                )
                .unwrap();
        }
        assert!(
            covered_state
                .durable_coverage(&route.app_id, &route.record_id)
                .unwrap()
                .is_some()
        );
        assert!(covered_state.outbox_message_retired(&original));
        let acknowledged =
            super::outgoing_projection(&covered_state, std::slice::from_ref(&original), later);
        assert_eq!(acknowledged.pending, 0);
        assert_eq!(acknowledged.blocked_expired, 0);
        assert_eq!(acknowledged.expiry_at_ms, None);
        let durable = client.durable().unwrap();
        let fresh_restored = StatePeer::restore(
            &durable,
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        let fresh =
            super::outgoing_projection(&fresh_restored, std::slice::from_ref(&original), now);
        assert_eq!(fresh.pending, 1);
        assert_eq!(fresh.blocked_expired, 0);
        assert_eq!(fresh.expiry_at_ms, Some(2_300_001));

        let mut restored = StatePeer::restore(
            &durable,
            room,
            authority.clone(),
            vec![authority, member],
            RevocationList::new(),
            later,
        )
        .unwrap();
        let mut outbox = vec![original];
        let blocked = super::outgoing_projection(&restored, &outbox, later);
        assert_eq!(blocked.pending, 1);
        assert_eq!(blocked.blocked_expired, 1);
        assert_eq!(blocked.expiry_at_ms, None);
        assert_eq!(
            super::runtime_phase("admitting", true, false, false, false, false),
            "ready",
            "outgoing expiry is not an admission failure"
        );
        assert_eq!(
            super::runtime_phase("admitting", true, true, false, false, false),
            "admitting",
            "a never-ready owner cannot bypass its initial persistence barrier"
        );
        assert_eq!(
            super::runtime_phase("admitting", true, true, true, false, false),
            "ready",
            "an established ready owner remains ready while committing later writes"
        );
        assert_eq!(super::admission_phase(&restored, true, None), "ready");

        for message in governor.bootstrap_messages(&authority_key, later).unwrap() {
            restored
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        authority_key.verifying_key().to_bytes(),
                    ),
                    message,
                    later,
                )
                .unwrap();
        }
        outbox = restored
            .reconcile_outbox(&outbox, &member_key, later)
            .unwrap();
        assert_eq!(outbox.len(), 2, "original custody plus fresh replacement");
        let renewed = super::outgoing_projection(&restored, &outbox, later);
        assert_eq!(renewed.blocked_expired, 0);
        assert_eq!(renewed.expiry_at_ms, Some(2_601_001));
    }
}
