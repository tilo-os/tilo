use super::*;
tilo_test_env::scenario! {
    fn delivery_admission_refuses_what_exceeds_its_resources_and_changes_nothing;

    phase delivery_admission_local_snapshot_quota_failure_leaves_governance_unchanged;
    {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_exact_admissions().unwrap();
        // Populate the real bounded index near its byte ceiling; no mocked admission/store.
        let full = DocumentAdmission {
            key: crate::admission::AdmissionKey {
                workspace: String::new(),
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

    phase delivery_admission_review_quota_rejected_pending_checkpoint_does_not_refresh_route;
    {
        let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
        let mut author = receiver.clone();
        let later = now + Duration::from_secs(301);
        let current_document = author.document("test.app", "tab:1").unwrap();
        let snapshot = author
            .publication_mut()
            .publish_document("test.app", "tab:1", current_document, &key, later)
            .unwrap();
        let origin = crate::fixture::origin(&key);
        receiver.select_exact_admissions().unwrap();
        let pending = receiver
            .receive_from_report(origin, snapshot, later)
            .unwrap();
        assert_eq!(
            pending.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::CheckpointCoverage)
        );
        assert_eq!(
            receiver.pending_count(),
            1,
            "the checkpoint waits for what it builds on"
        );
        let rows = receiver.rows();
        let watermarks = receiver.replay_floors();
        // Fill the actual bounded index so the otherwise valid pending checkpoint
        // fails insertion inside a successful governance-triggered drain.
        receiver
            .core
            .exact_admissions
            .insert(&DocumentAdmission {
                key: crate::admission::AdmissionKey {
                    workspace: String::new(),
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
            .receive_from_report(origin, author.workspace_message(), later)
            .unwrap();
        assert_eq!(report.summary().rejected, 1);
        assert!(report.settled().is_empty());
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.rows(), rows);
        assert_eq!(receiver.replay_floors(), watermarks);
        assert_eq!(receiver.core.exact_admissions.encode().unwrap(), index);
    }

    phase delivery_admission_restore_refuses_index_count_bomb_and_unbound_evidence;
    {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_exact_admissions().unwrap();
        let document = peer.document("test.app", "tab:1").unwrap();
        peer.publication_mut()
            .publish_document("test.app", "tab:1", document, &key, now)
            .unwrap();
        let literal = peer.durable().unwrap();
        let restore = |bytes: &[u8]| {
            StatePeer::restore(
                bytes,
                peer.workspace_id(),
                peer.owner_certificates()[0].clone(),
                peer.certificates(),
                RevocationList::new(),
                now,
            )
        };
        assert!(restore(&literal).is_ok());
        let mut value: Durable<'_> = postcard::from_bytes(&literal).unwrap();
        let exact = value.exact.unwrap();
        value.exact = Some(&[0x81, 0x80, 0x04]);
        assert!(restore(&postcard::to_allocvec(&value).unwrap()).is_err());
        let mut index = ExactAdmissions::decode(exact).unwrap();
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
        value.exact = Some(&exact);
        assert!(restore(&postcard::to_allocvec(&value).unwrap()).is_err());
    }
}
