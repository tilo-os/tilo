use super::*;
use crate::action_host::scoped_fixture::ScopedAppFixture;

#[tokio::test(flavor = "current_thread")]
async fn exact_startup_replay_uses_actual_signed_baseline_without_any_store_write() {
    let mut fixture = ScopedAppFixture::new("dev.qos.word-processor");
    let pending = fixture.next_startup_signature().await;
    let time = fixture.host.registered_policy_time().clone();
    let generation = fixture.owner.committed_generation();
    let path = fixture.owner.root().join("session.bin");
    let disk = std::fs::read(&path).unwrap();
    let outgoing = fixture
        .owner
        .outgoing()
        .iter()
        .map(|p| p.bytes.to_vec())
        .collect::<Vec<_>>();
    let root = fixture.owner.root().to_owned();
    let mut replay_request = None;
    let (complete, result) = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::BeforeWrite,
        || {
            pending.0.apply_scoped_fenced(crate::owner_commit::execution_deadline(), |route, certificate, mutation, authorization| {
            let authorization = authorization.expect("genuine mounted required publication retains custody");
            let mut wrong_route = route.clone();
            wrong_route.app_id.push_str(".wrong");
            assert!(fixture.owner.replay_exact_startup_document(&wrong_route, certificate, &mutation, authorization, &time).is_err());
            let mut changed = mutation.clone();
            match &mut changed { DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes.push(0) }
            assert!(fixture.owner.replay_exact_startup_document(route, certificate, &changed, authorization, &time).is_err());
            let replay = fixture.owner.replay_exact_startup_document(route, certificate, &mutation, authorization, &time)
                .map_err(HostDocumentTurnError::Refused)?;
            assert_eq!(replay.0.payload, mutation.encode().unwrap());
            assert!(outgoing.iter().any(|bytes| matches!(postcard::from_bytes::<NeighbourMsg>(bytes), Ok(NeighbourMsg::AppDocument { envelope, .. }) if postcard::to_stdvec(envelope.as_ref()).unwrap() == postcard::to_stdvec(&replay.0).unwrap())));
            assert_eq!(replay.1.generation, generation);
            assert_eq!(replay.1.aggregate_hash, *blake3::hash(&disk).as_bytes());
            replay_request = Some(OwnerCommitRequest { room: route.cluster_id.to_string(), operation: CommitOperation::Document { app: route.app_id.clone(), record: route.record.clone().unwrap_or_default(), envelope: Box::new(replay.0.clone()) } });
            Ok(replay)
        })
        },
    );
    assert!(
        result.is_ok(),
        "actual retained baseline must replay: {result:?}"
    );
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(std::fs::read(&path).unwrap(), disk);
    assert_eq!(
        fixture
            .owner
            .outgoing()
            .iter()
            .map(|p| p.bytes.to_vec())
            .collect::<Vec<_>>(),
        outgoing
    );
    let service = fixture.signing();
    assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
    let request = replay_request.unwrap();
    let receipt = service.commit(request.clone()).await.unwrap();
    receipt.validate_for(&request).unwrap();
    assert_eq!(service.shared.pending_exact_receipts().unwrap(), 0);
    assert!(
        service.commit(request).await.is_err(),
        "one exact receipt cannot be reused"
    );
    complete();
}

#[tokio::test(flavor = "current_thread")]
async fn exact_startup_replay_refuses_the_retired_original_owner_origin() {
    let mut fixture = ScopedAppFixture::new("dev.qos.word-processor");
    let pending = fixture.next_startup_signature().await;
    let time = fixture.host.registered_policy_time().clone();
    let generation = fixture.owner.committed_generation();
    let disk = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    fixture.retire_policy_origin();
    let (complete, result) = pending.0.apply_scoped_fenced(
        crate::owner_commit::execution_deadline(),
        |route, certificate, mutation, authorization| {
            fixture
                .owner
                .replay_exact_startup_document(
                    route,
                    certificate,
                    &mutation,
                    authorization.unwrap(),
                    &time,
                )
                .map_err(HostDocumentTurnError::Refused)
        },
    );
    assert!(result.is_err());
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        disk
    );
    assert_eq!(
        fixture.signing().shared.pending_exact_receipts().unwrap(),
        0
    );
    complete();
}

#[tokio::test(flavor = "current_thread")]
async fn exact_startup_replay_refuses_actual_original_invocation_contributors() {
    let mut fixture = ScopedAppFixture::new("dev.qos.notes");
    fixture.wait_action("new-page").await;
    for _ in 0..10 {
        fixture.pump().await.unwrap();
    }
    let time = fixture.host.registered_policy_time().clone();
    let generation = fixture.owner.committed_generation();
    let disk = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let record = fixture.record_id();
    let before_local = fixture
        .records()
        .read(&record, |doc| doc.clone().save())
        .unwrap();
    let invocation = fixture
        .invoke_holding_documents(
            "new-page",
            serde_json::Map::from_iter([(
                "value".into(),
                serde_json::json!("Replay contributor proof"),
            )]),
        )
        .await;
    let pending = fixture.next_startup_signature().await;
    assert_ne!(
        fixture
            .records()
            .read(&record, |doc| doc.clone().save())
            .unwrap(),
        before_local,
        "the real original invocation must produce actual local history before final refusal"
    );
    let (complete, result) = pending.0.apply_scoped_fenced(
        crate::owner_commit::execution_deadline(),
        |route, certificate, mutation, authorization| {
            let authorization = authorization.expect("actual required job publication custody");
            let mut contributors = 0;
            authorization
                .validate_owner_contributors(|request, captured_route, token| {
                    assert_eq!(request.wire_id, invocation);
                    assert_eq!(captured_route, route);
                    let original = token
                        .downcast_ref::<qos_action_broker::collab_broker_capnp::OriginalInvocation>(
                        )
                        .expect("real broker-issued original token");
                    assert_eq!(original.request(), request);
                    original.check_custody()?;
                    contributors += 1;
                    Ok(())
                })
                .unwrap();
            assert!(
                contributors > 0,
                "new-page must carry its real original caller"
            );
            let error = fixture
                .owner
                .replay_exact_startup_document(route, certificate, &mutation, authorization, &time)
                .unwrap_err();
            assert!(error.contains("contributors"), "wrong refusal: {error}");
            Err(HostDocumentTurnError::Refused(error))
        },
    );
    assert!(result.is_err());
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        disk
    );
    assert_eq!(
        fixture.signing().shared.pending_exact_receipts().unwrap(),
        0
    );
    complete();
}

#[tokio::test(flavor = "current_thread")]
async fn exact_startup_replay_refuses_uncommitted_factory_body_with_genuine_sdk_binding() {
    let mut fixture = ScopedAppFixture::with_uncommitted_factory_data("dev.qos.notes");
    let pending = fixture.next_signature_for_selected_record().await;
    let time = fixture.host.registered_policy_time().clone();
    let generation = fixture.owner.committed_generation();
    let disk = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let (complete, result) = pending.0.apply_scoped_fenced(
        crate::owner_commit::execution_deadline(),
        |route, certificate, mutation, authorization| {
            let authorization = authorization.expect("genuine SDK startup custody");
            authorization.check_binding(route, &mutation).unwrap();
            let mut contributors = 0;
            authorization
                .validate_owner_contributors(|_, _, _| {
                    contributors += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                contributors, 0,
                "absence of contributors must never confer canonicality"
            );
            let error = fixture
                .owner
                .replay_exact_startup_document(route, certificate, &mutation, authorization, &time)
                .unwrap_err();
            assert!(
                error.contains("not the exact retained"),
                "wrong refusal: {error}"
            );
            Err(HostDocumentTurnError::Refused(error))
        },
    );
    assert!(result.is_err());
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        disk
    );
    assert_eq!(
        fixture.signing().shared.pending_exact_receipts().unwrap(),
        0
    );
    complete();
}
