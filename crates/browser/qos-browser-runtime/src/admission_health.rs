use qos_peer_state::StatePeer;
use web_time::SystemTime;

pub(crate) fn durable_admission_failure(
    state: &StatePeer,
    device: [u8; 32],
    now: SystemTime,
    admission_failure: Option<String>,
) -> Option<String> {
    state.incomplete_reason().or(admission_failure).or_else(|| {
        state
            .has_governance()
            .then(|| state.permissions(device, now).err())
            .flatten()
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
    use qos_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
    use qos_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, sign_device_cert,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
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
            app_id: "dev.qos.word-processor".into(),
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

    #[derive(Serialize, Deserialize)]
    struct DurableDocumentFixture {
        route: RouteFixture,
        bytes: Vec<u8>,
        watermarks: Vec<([u8; 32], u64)>,
        encryption_required: bool,
        covered: BTreeMap<String, u64>,
        covered_snapshot: Option<Vec<u8>>,
    }

    #[derive(Serialize, Deserialize)]
    struct RouteFixture {
        app: String,
        record: String,
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
            super::durable_admission_failure(&state, member.device_public_key, now, None),
            None,
            "missing governance is a bootstrap prerequisite, not a permission failure"
        );
        assert_eq!(
            super::durable_admission_failure(
                &state,
                member.device_public_key,
                now,
                Some("persisted admission failed".into()),
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
            super::durable_admission_failure(&state, member.device_public_key, now, None),
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
            super::durable_admission_failure(&denied, member.device_public_key, now, None,),
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

        let mut durable: DurableFixture =
            postcard::from_bytes(&admitted.durable().unwrap()).unwrap();
        durable.room_bytes = None;
        let restored = StatePeer::restore(
            &postcard::to_allocvec(&durable).unwrap(),
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
            super::durable_admission_failure(&denied, member.device_public_key, now, None);
        assert!(!denied.rows().is_empty());
        assert_eq!(
            super::admission_phase(&denied, true, failure.as_deref()),
            "failed"
        );
    }
}
