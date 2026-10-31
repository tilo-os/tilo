use super::*;
#[test]
fn delivery_admission_local_snapshot_quota_failure_leaves_governance_unchanged() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    peer.select_admission_v5().unwrap();
    // Populate the real bounded index near its byte ceiling; no mocked admission/store.
    let full = DocumentAdmission {
        key: crate::admission::AdmissionKey {
            room: String::new(),
            app: String::new(),
            record: "x".repeat(crate::MAX_EXACT_ADMISSION_BYTES - 100),
            author: [0; 32],
            sequence: 0,
        },
        operation_id: [1; 32],
    };
    peer.core.exact_admissions.insert(&full).unwrap();
    let before = peer.durable().unwrap();
    let document = peer.document("test.app", "tab:1").unwrap();
    assert!(
        peer.publication_mut()
            .publish_document("test.app", "tab:1", document, &key, now)
            .is_err()
    );
    assert!(
        peer.durable().unwrap() == before,
        "quota failure changed governance before admission"
    );
}
#[test]
fn delivery_admission_review_quota_rejected_pending_checkpoint_does_not_refresh_route() {
    let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
    let mut author = receiver.clone();
    let later = now + Duration::from_secs(301);
    let current_document = author.document("test.app", "tab:1").unwrap();
    let snapshot = author
        .publication_mut()
        .publish_document("test.app", "tab:1", current_document, &key, later)
        .unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.select_admission_v5().unwrap();
    let pending = receiver
        .receive_from_report(origin, snapshot, later)
        .unwrap();
    assert_eq!(
        pending.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::CheckpointCoverage)
    );
    assert_eq!(receiver.pending_count(), 1);
    assert!(!receiver.fresh_checkpoint_for("test.app", "tab:1", later));
    let checkpoint_times = receiver.core.checkpoint_times.clone();
    let rows = receiver.rows();
    let watermarks = receiver.replay_floors();
    // Fill the actual bounded index so the otherwise valid pending checkpoint
    // fails insertion inside a successful governance-triggered drain.
    receiver
        .core
        .exact_admissions
        .insert(&DocumentAdmission {
            key: crate::admission::AdmissionKey {
                room: String::new(),
                app: String::new(),
                record: "x".repeat(crate::MAX_EXACT_ADMISSION_BYTES - 100),
                author: [0; 32],
                sequence: 0,
            },
            operation_id: [1; 32],
        })
        .unwrap();
    let index = receiver.core.exact_admissions.encode().unwrap();
    let report = receiver
        .receive_from_report(origin, author.room_message(), later)
        .unwrap();
    assert_eq!(report.summary().rejected, 1);
    assert!(report.settled().is_empty());
    assert_eq!(receiver.pending_count(), 0);
    assert_eq!(receiver.rows(), rows);
    assert_eq!(receiver.replay_floors(), watermarks);
    assert_eq!(receiver.core.exact_admissions.encode().unwrap(), index);
    assert!(
        !receiver.fresh_checkpoint_for("test.app", "tab:1", later),
        "quota-rejected pending checkpoint refreshed the route"
    );
    assert_eq!(receiver.core.checkpoint_times, checkpoint_times);
}
#[test]
fn delivery_admission_restore_refuses_index_count_bomb_and_unbound_evidence() {
    let literal = include_str!("../../tests/fixtures/delivery-admission/native-v5.postcard.hex")
        .split_ascii_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect::<Vec<_>>();
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let value: DurableV5<'_> = postcard::from_bytes(&literal).unwrap();
    let bomb = postcard::to_allocvec(&DurableV5 {
        version: 5,
        base: value.base,
        exact: &[0x81, 0x80, 0x04],
    })
    .unwrap();
    assert!(
        StatePeer::restore(
            &bomb,
            peer.room(),
            peer.owner_certificates()[0].clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
    let value: DurableV5<'_> = postcard::from_bytes(&literal).unwrap();
    let mut index = ExactAdmissions::decode(value.exact).unwrap();
    let first = index
        .iter()
        .next()
        .map(|(key, id)| DocumentAdmission {
            key: key.clone(),
            operation_id: *id,
        })
        .unwrap();
    let mut invalid = first;
    invalid.key.sequence += 1;
    index.insert(&invalid).unwrap();
    let exact = index.encode().unwrap();
    let bytes = postcard::to_allocvec(&DurableV5 {
        version: 5,
        base: value.base,
        exact: &exact,
    })
    .unwrap();
    assert!(
        StatePeer::restore(
            &bytes,
            peer.room(),
            peer.owner_certificates()[0].clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
}
