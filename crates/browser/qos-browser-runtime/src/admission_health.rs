use qos_peer_state::StatePeer;
use web_time::SystemTime;

pub(crate) fn durable_admission_failure(
    state: &StatePeer,
    device: [u8; 32],
    now: SystemTime,
    admission_failure: Option<String>,
) -> Option<String> {
    state
        .incomplete_reason()
        .or(admission_failure)
        .or_else(|| {
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
                    room_changes: seed_room(
                        room,
                        &authority,
                        &[authority.clone(), member.clone()],
                    )
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
            super::durable_admission_failure(
                &denied,
                member.device_public_key,
                now,
                None,
            ),
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
                    room_changes: seed_room(
                        room,
                        &authority,
                        &[authority.clone(), member.clone()],
                    )
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
        let failure = super::durable_admission_failure(
            &denied,
            member.device_public_key,
            now,
            None,
        );
        assert!(!denied.rows().is_empty());
        assert_eq!(
            super::admission_phase(&denied, true, failure.as_deref()),
            "failed"
        );
    }
}
