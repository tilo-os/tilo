use super::*;
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

fn mutation(owner: &SessionOwner, key: &str) -> collab_types::DocumentMutation {
    let mut document =
        AutoCommit::load(&owner.peer().document("dev.qos.chat", "").unwrap()).unwrap();
    document.put(ROOT, key, true).unwrap();
    document.commit();
    collab_types::DocumentMutation::Snapshot(document.save())
}

// This uses the explicit authority-transition fixture, not an MLS repair or a
// plaintext downgrade. The owner, its new durable generation, application host,
// registered signing queues and invocation journal are the production objects.
#[tokio::test(flavor = "current_thread")]
async fn action_lifecycle_recovery_rejects_old_queue_and_advances_fresh_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let committed = crate::committed_authority::CommittedAuthority::new(&owner);
    let mut runtime = ActionRuntime::new(committed.clone());
    let host = LocalHost::start(&owner).await.unwrap();
    let mut connected = host.connect(&owner).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !matches!(connected.recv().await.unwrap(), Inbound::Ready { .. }) {}
    })
    .await
    .unwrap();
    let mut session = Some(connected);
    let mut action_error = None;
    let mut cadence = CheckpointCadence::new();
    refresh_actions(
        &mut owner,
        &mut runtime,
        &mut session,
        None,
        &mut action_error,
        None,
        &mut cadence,
    )
    .await;
    assert!(action_error.is_none());
    let old_time = committed.read().unwrap().view().policy_time().unwrap();
    let route =
        ui::collaboration::RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
    let old_signing = runtime.signing.clone();
    let old_signer = old_signing.bind(route.clone(), owner.identity.native_cert.clone());
    let old_mutation = mutation(&owner, "must-not-commit-retired-runtime");
    let old_waiting = tokio::spawn(old_signer.commit(&route, old_mutation));
    let queued_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while old_signing.retirement_ready().unwrap() {
        assert!(
            tokio::time::Instant::now() < queued_deadline,
            "old request did not enter its bounded queue"
        );
        tokio::task::yield_now().await;
    }

    crate::committed_authority::enter_test_recovery(&committed);
    assert!(old_time.now_ns().is_err());
    refresh_actions(
        &mut owner,
        &mut runtime,
        &mut session,
        None,
        &mut action_error,
        None,
        &mut cadence,
    )
    .await;
    assert!(action_error.is_none());
    assert!(runtime.current_actions().is_none());
    assert!(!owner.root().join("actions/recovery.json").exists());
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("lifecycle-recovery", b"retained", false)
        })
        .unwrap()
        .unwrap();
    let recovered_time = committed.read().unwrap().view().policy_time().unwrap();
    assert!(!old_time.same_origin(&recovered_time));
    // The existing subscription is already Ready: no new Ready event or app
    // restart is needed for the normal owner-loop boundary to recreate workers.
    assert!(session.as_ref().unwrap().is_ready());
    refresh_actions(
        &mut owner,
        &mut runtime,
        &mut session,
        None,
        &mut action_error,
        None,
        &mut cadence,
    )
    .await;

    let new_signer = runtime
        .signing
        .bind(route.clone(), owner.identity.native_cert.clone());
    let fresh_mutation = mutation(&owner, "fresh-runtime-committed");
    let mut fresh_waiting = tokio::spawn(new_signer.commit(&route, fresh_mutation));
    let fresh_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! {
                result = &mut fresh_waiting => break result.unwrap(),
                Some(pending) = runtime.signatures.recv() => {
                    let update = committed.begin(&owner).unwrap();
                    let (complete, _) = pending.apply(&mut owner);
                    update.publish(&owner).unwrap();
                    complete();
                }
            }
        }
    })
    .await;
    let old_result = tokio::time::timeout(Duration::from_secs(1), old_waiting).await;
    // Release real workers before reporting a failed expectation.
    if fresh_result.is_err() {
        fresh_waiting.abort();
        let _ = fresh_waiting.await;
    }
    if let Ok(Ok(envelope)) = &fresh_result {
        runtime
            .signing
            .commit(OwnerCommitRequest {
                room: owner.peer().room().into(),
                operation: CommitOperation::Document {
                    app: "dev.qos.chat".into(),
                    record: String::new(),
                    envelope: Box::new(envelope.clone()),
                },
            })
            .await
            .unwrap();
    }
    let cleanup = runtime.actions.as_mut().unwrap().close(&owner);
    drop(runtime);
    session.take().unwrap().close().await;
    host.shutdown().await.unwrap();

    assert!(action_error.is_none(), "{action_error:?}");
    assert!(
        old_result
            .expect("retired request receives a bounded refusal")
            .unwrap()
            .is_err()
    );
    assert!(
        fresh_result.expect("fresh owner queue progressed").is_ok(),
        "fresh runtime retained the retired source"
    );
    cleanup.unwrap();
    let document = AutoCommit::load(&owner.peer().document("dev.qos.chat", "").unwrap()).unwrap();
    use ui::automerge::ReadDoc;
    assert!(
        document
            .get(ROOT, "must-not-commit-retired-runtime")
            .unwrap()
            .is_none()
    );
    assert!(
        document
            .get(ROOT, "fresh-runtime-committed")
            .unwrap()
            .is_some()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn action_lifecycle_recovery_preserves_terminal_signing_failure() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let committed = crate::committed_authority::CommittedAuthority::new(&owner);
    let mut runtime = ActionRuntime::new(committed.clone());
    runtime.start_current(&mut owner, None, None).unwrap();
    runtime.signing.fail("original terminal signing failure");
    crate::committed_authority::enter_test_recovery(&committed);
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("lifecycle-recovery", b"retained", false)
        })
        .unwrap()
        .unwrap();
    let result = runtime.start_current(&mut owner, None, None);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("original terminal signing failure")
    );
    assert_eq!(
        runtime.signing.health().unwrap_err(),
        "original terminal signing failure"
    );
    assert!(runtime.current_actions().is_none());
    runtime.actions.as_mut().unwrap().close(&owner).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn action_lifecycle_retirement_refuses_every_queue_despite_terminal_health() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let committed = crate::committed_authority::CommittedAuthority::new(&owner);
    let mut runtime = ActionRuntime::new(committed.clone());
    runtime.start_current(&mut owner, None, None).unwrap();
    let route =
        ui::collaboration::RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
    let signer = runtime
        .signing
        .bind(route.clone(), owner.identity.native_cert.clone());
    let mut first = Box::pin(tokio::task::unconstrained(
        signer.commit(&route, mutation(&owner, "never-first")),
    ));
    let mut second = Box::pin(tokio::task::unconstrained(
        signer.commit(&route, mutation(&owner, "never-second")),
    ));
    let invocation = uuid::Uuid::new_v4();
    let receipt = qos_desktop::WorkspaceEffectReceipt {
        invocation: serde_json::from_value(serde_json::json!(invocation)).unwrap(),
        actor: hex::encode(owner.peer().authority().user_public_key),
        context: Default::default(),
        status: serde_json::from_value(serde_json::json!({"status":"succeeded"})).unwrap(),
        completed_at: 42,
    };
    let mut ordinary = Box::pin(tokio::task::unconstrained(runtime.signing.commit(
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::EffectReceipt {
                invocation,
                redacted: serde_json::to_vec(&receipt).unwrap(),
            },
        },
    )));
    // Poll each real producer through its enqueue to the owner-start milestone.
    // There are only three requests against the production 32-permit bounds.
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        assert!(ordinary.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let before = owner.status();
    runtime.signing.fail("original terminal signing failure");
    crate::committed_authority::enter_test_recovery(&committed);
    let refusal = runtime.retire();
    let results = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(first, second, ordinary)
    })
    .await;
    let preserved = runtime.signing.health();
    let cleanup = runtime.actions.as_mut().unwrap().close(&owner);
    drop(runtime);

    assert_eq!(refusal.unwrap_err(), "original terminal signing failure");
    assert_eq!(preserved.unwrap_err(), "original terminal signing failure");
    let (first, second, ordinary) =
        results.expect("every never-started caller must receive refusal");
    assert_eq!(first.unwrap_err(), "original terminal signing failure");
    assert_eq!(second.unwrap_err(), "original terminal signing failure");
    assert!(ordinary.is_err());
    assert_eq!(owner.status(), before);
    cleanup.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn action_lifecycle_recovery_retains_exact_postcommit_receipt_custody() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let committed = crate::committed_authority::CommittedAuthority::new(&owner);
    let mut runtime = ActionRuntime::new(committed.clone());
    runtime.start_current(&mut owner, None, None).unwrap();
    let route =
        ui::collaboration::RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
    let signer = runtime
        .signing
        .bind(route.clone(), owner.identity.native_cert.clone());
    let waiting =
        tokio::spawn(signer.commit(&route, mutation(&owner, "committed-before-retirement")));
    let pending = runtime.signatures.recv().await.unwrap();
    let update = committed.begin(&owner).unwrap();
    let (complete, result) = pending.apply(&mut owner);
    result.unwrap();
    update.publish(&owner).unwrap();
    complete();
    let _exact_unconsumed = waiting.await.unwrap().unwrap();
    assert!(!runtime.signing.retirement_ready().unwrap());

    crate::committed_authority::enter_test_recovery(&committed);
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("lifecycle-recovery", b"retained", false)
        })
        .unwrap()
        .unwrap();
    let before = owner.status();
    let result = runtime.start_current(&mut owner, None, None);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("unresolved signing custody")
    );
    assert!(!runtime.signing.retirement_ready().unwrap());
    assert!(runtime.current_actions().is_none());
    assert_eq!(owner.status(), before);
    runtime.actions.as_mut().unwrap().close(&owner).unwrap();
}
