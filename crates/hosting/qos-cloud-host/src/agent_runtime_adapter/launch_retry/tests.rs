//! Actual Store/Source and Unix task/descriptor custody. Runtime facts are inert
//! candidate data; these tests do not construct a managed-runtime/process witness.
use super::*;
use crate::agent_runtime_owner::{
    LaunchCandidateTestClaim, launch_stage_test_admit, launch_stage_test_enrolled,
    launch_stage_test_intent,
};
use crate::https_owner::{CloudStoreSource, Current};
use atomic_file::test_support::Checkpoint;
use qos_peer_state::owner_commit::ManagedRuntimeFactsV2;
use std::io::Read;

struct Fixture {
    dir: tempfile::TempDir,
    owner: RuntimeOwner,
    attempt: Arc<LiveAttempt>,
}
fn fixture(id: u128) -> Fixture {
    let (dir, mut store, device) = launch_stage_test_enrolled();
    let clock = OwnerPolicyClock::new().unwrap();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    store
        .activate_delivery_storage(clock.now_ns().unwrap())
        .unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("actual Source required");
    };
    let owner =
        RuntimeOwner::open_with_source(dir.path(), "private-runtime", clock, time.clone(), source)
            .unwrap();
    let mut store = owner.store_for(&time).unwrap();
    let caller = store.agent_caller(device).unwrap();
    let intent = launch_stage_test_intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admitted) =
        launch_stage_test_admit(&mut store, &caller, Uuid::from_u128(id), &intent).unwrap()
    else {
        panic!("actual fresh admission required");
    };
    let attempt = Arc::new(LiveAttempt {
        policy_time: time,
        ids: admitted.ids.clone(),
        caller: caller.certificate().clone(),
        intent,
        custody: admitted.into_custody(),
        launch: Mutex::new(LiveLaunchSlot::Empty),
    });
    owner
        .live
        .lock()
        .unwrap()
        .attempts
        .insert(Uuid::from_u128(id), attempt.clone());
    drop(store);
    Fixture {
        dir,
        owner,
        attempt,
    }
}
fn facts() -> ManagedRuntimeFactsV2 {
    ManagedRuntimeFactsV2 {
        runtime_epoch: Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    }
}
fn fail(
    f: &Fixture,
    checkpoint: Checkpoint,
    retry: Option<QuiescentLaunchRetry>,
) -> FailedLaunchAppend {
    let mut store = f.owner.store_for(&f.attempt.policy_time).unwrap();
    let result = crate::storage::test_support::with_save_failure(f.dir.path(), checkpoint, || {
        store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), retry)
    });
    let LaunchCandidateTestClaim::Failed(failed) = result else {
        panic!("actual failed commit required")
    };
    assert!(
        failed
            .error()
            .downcast_ref::<crate::StoreCommitFailure>()
            .is_some()
    );
    assert!(store.sole_admitted_spawn(&f.attempt.custody.id()).is_err());
    failed
}
async fn make_connector(attempt: Arc<LiveAttempt>) -> (GatewayConnector, UnixStream) {
    let (model, peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let task = tokio::task::spawn_local(async move { Ok(model) });
    let connector = GatewayConnector { attempt, task };
    while !connector.is_finished() {
        tokio::task::yield_now().await;
    }
    (connector, peer)
}
fn assert_closed(peer: &mut UnixStream) {
    assert_eq!(
        peer.read(&mut [0u8; 1]).unwrap(),
        0,
        "exact returned descriptor must be closed"
    );
}
fn run(test: impl std::future::Future<Output = ()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, test);
}

#[test]
fn agent_launch_retry_joined_failed_call_resumes_original_once() {
    run(async {
        let f = fixture(930);
        let failed = fail(&f, Checkpoint::PublishSubmitted, None);
        assert!(failed.belongs_to(&f.attempt));
        let (connector, mut peer) = make_connector(f.attempt.clone()).await;
        let joined = connector.join().await.unwrap();
        assert!(
            matches!(peer.read(&mut [0u8;1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        joined.close_and_park(&f.owner, failed).unwrap();
        assert_closed(&mut peer);
        let retry = take_retry(&f.attempt).unwrap().unwrap();
        assert!(take_retry(&f.attempt).unwrap().is_none());
        let mut store = f.owner.store_for(&f.attempt.policy_time).unwrap();
        retry.validate(&f.attempt.custody, &store).unwrap();
        let before = store.state.generation;
        let LaunchCandidateTestClaim::Committed(true, binding) =
            store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), Some(retry))
        else {
            panic!("one resumed barrier required")
        };
        assert_eq!(binding.generation, before + 1);
        assert_eq!(binding.launch, f.attempt.ids.launch_id);
        assert_eq!(store.state.generation, before + 1);
        assert!(matches!(
            store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), None),
            LaunchCandidateTestClaim::Committed(_, _)
        ));
        assert_eq!(
            store.state.generation,
            before + 1,
            "existing unique fact must not append again"
        );
        assert!(
            take_permit(&f.attempt).is_err(),
            "inert core never mints a runtime permit"
        );
    });
}

#[test]
fn agent_launch_retry_failure_matrix_and_repeated_failure_use_one_slot() {
    run(async {
        for (n, checkpoint, can_retry) in [
            (0, Checkpoint::BeforePublish, true),
            (1, Checkpoint::PublishSubmitted, true),
            (2, Checkpoint::AfterPublish, false),
            (3, Checkpoint::BeforeCommit, false),
        ] {
            let f = fixture(940 + n);
            let failed = fail(&f, checkpoint, None);
            let (connector, mut peer) = make_connector(f.attempt.clone()).await;
            connector
                .join()
                .await
                .unwrap()
                .close_and_park(&f.owner, failed)
                .unwrap();
            assert_closed(&mut peer);
            let retry = take_retry(&f.attempt).unwrap().unwrap();
            let mut store = f.owner.store_for(&f.attempt.policy_time).unwrap();
            let before = store.state.generation;
            if can_retry {
                assert!(
                    store
                        .sole_admitted_spawn(&f.attempt.custody.id())
                        .unwrap()
                        .is_some()
                );
                drop(store);
                let failed = fail(&f, Checkpoint::PublishSubmitted, Some(retry));
                assert!(take_retry(&f.attempt).unwrap().is_none());
                let (connector, mut peer) = make_connector(f.attempt.clone()).await;
                connector
                    .join()
                    .await
                    .unwrap()
                    .close_and_park(&f.owner, failed)
                    .unwrap();
                assert_closed(&mut peer);
                let retry = take_retry(&f.attempt).unwrap().unwrap();
                let mut store = f.owner.store_for(&f.attempt.policy_time).unwrap();
                assert!(matches!(
                    store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), Some(retry)),
                    LaunchCandidateTestClaim::Committed(_, _)
                ));
                assert_eq!(store.state.generation, before + 1);
            } else {
                assert!(
                    store
                        .sole_admitted_spawn(&f.attempt.custody.id())
                        .unwrap()
                        .is_none()
                );
                assert!(matches!(
                    store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), Some(retry)),
                    LaunchCandidateTestClaim::Committed(_, _)
                ));
                assert_eq!(
                    store.state.generation, before,
                    "published barrier must not append again"
                );
            }
        }
    });
}

#[test]
fn agent_launch_retry_wrong_attempt_and_cancelled_connector_refuse() {
    run(async {
        let f = fixture(950);
        let other = fixture(951);
        let failed = fail(&f, Checkpoint::PublishSubmitted, None);
        let (connector, mut peer) = make_connector(other.attempt.clone()).await;
        assert!(
            connector
                .join()
                .await
                .unwrap()
                .close_and_park(&other.owner, failed)
                .is_err()
        );
        assert_closed(&mut peer);
        assert!(take_retry(&f.attempt).unwrap().is_none());
        assert!(take_retry(&other.attempt).unwrap().is_none());
        let (model, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let task = tokio::task::spawn_local(async move {
            let held = model;
            std::future::pending::<()>().await;
            Ok(held)
        });
        let connector = GatewayConnector {
            attempt: f.attempt.clone(),
            task,
        };
        tokio::task::yield_now().await;
        assert!(!connector.is_finished());
        connector.abort_and_join().await;
        assert_closed(&mut peer);
        assert!(take_retry(&f.attempt).unwrap().is_none());
    });
}

#[test]
fn agent_launch_retry_current_refusal_restores_original_provenance() {
    run(async {
        let f = fixture(960);
        let failed = fail(&f, Checkpoint::PublishSubmitted, None);
        let provenance = failed
            .error()
            .downcast_ref::<crate::StoreCommitFailure>()
            .unwrap() as *const _;
        let (connector, mut peer) = make_connector(f.attempt.clone()).await;
        connector
            .join()
            .await
            .unwrap()
            .close_and_park(&f.owner, failed)
            .unwrap();
        assert_closed(&mut peer);
        let before = std::fs::read(f.dir.path().join("state.json")).unwrap();
        // This real owner deliberately has no installed VerifiedManagedRuntime.
        // Its genuine current prerequisite denial must keep exactly the same token.
        assert!(matches!(
            f.owner.prepare_launch(&f.attempt),
            Err(PrepareLaunchError::Unavailable(_))
        ));
        let retry = take_retry(&f.attempt).unwrap().unwrap();
        assert!(std::ptr::eq(
            provenance,
            retry
                .failed
                .error()
                .downcast_ref::<crate::StoreCommitFailure>()
                .unwrap()
        ));
        let other = fixture(961);
        let mut other_store = other.owner.store_for(&other.attempt.policy_time).unwrap();
        let LaunchCandidateTestClaim::Refused {
            retry: Some(retry), ..
        } = other_store.claim_launch_candidate_unqualified_test(&f.attempt, facts(), Some(retry))
        else {
            panic!("another Source/time cannot consume the original retry");
        };
        drop(other_store);
        f.owner.retain_retry(&f.attempt, retry).unwrap();
        assert!(other.owner.prepare_launch(&f.attempt).is_err());
        assert!(matches!(
            *f.attempt.launch.lock().unwrap(),
            LiveLaunchSlot::Retry(_)
        ));
        f.owner.policy_context().close();
        assert!(f.attempt.policy_time.now_ns().is_err());
        assert!(f.owner.prepare_launch(&f.attempt).is_err());
        let retry = take_retry(&f.attempt).unwrap().unwrap();
        assert!(std::ptr::eq(
            provenance,
            retry
                .failed
                .error()
                .downcast_ref::<crate::StoreCommitFailure>()
                .unwrap()
        ));
        assert_eq!(
            std::fs::read(f.dir.path().join("state.json")).unwrap(),
            before
        );
    });
}

#[test]
fn agent_launch_retry_same_contents_in_replacement_arc_are_not_original() {
    run(async {
        let f = fixture(962);
        let failed = fail(&f, Checkpoint::PublishSubmitted, None);
        let Fixture {
            dir: _dir,
            owner,
            attempt,
        } = f;
        let id = attempt.custody.id().invocation();
        owner.live.lock().unwrap().attempts.remove(&id);
        let moved = match Arc::try_unwrap(attempt) {
            Ok(value) => value,
            Err(_) => panic!("sole original strong custody required"),
        };
        let replacement = Arc::new(moved); // Exact values and consumed atomic, different allocation.
        owner
            .live
            .lock()
            .unwrap()
            .attempts
            .insert(id, replacement.clone());
        assert!(!failed.belongs_to(&replacement));
        let (connector, mut peer) = make_connector(replacement.clone()).await;
        assert!(
            connector
                .join()
                .await
                .unwrap()
                .close_and_park(&owner, failed)
                .is_err()
        );
        assert_closed(&mut peer);
        assert!(take_retry(&replacement).unwrap().is_none());
        let mut store = owner.store_for(&replacement.policy_time).unwrap();
        assert!(matches!(
            store.claim_launch_candidate_unqualified_test(&replacement, facts(), None),
            LaunchCandidateTestClaim::Refused { retry: None, .. }
        ));
    });
}

#[test]
fn agent_launch_retry_shutdown_joins_already_returned_descriptor_and_panic() {
    run(async {
        let f = fixture(963);
        let (connector, mut peer) = make_connector(f.attempt.clone()).await;
        connector.abort_and_join().await;
        assert_closed(&mut peer);
        let (model, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let task = tokio::task::spawn_local(async move {
            let _held = model;
            panic!("actual connector panic");
            #[allow(
                unreachable_code,
                reason = "test/stub closure keeps its declared return type after a diverging call"
            )]
            Ok(UnixStream::pair().unwrap().0)
        });
        let connector = GatewayConnector {
            attempt: f.attempt.clone(),
            task,
        };
        while !connector.is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(connector.join().await.is_err());
        assert_closed(&mut peer);
        assert!(take_retry(&f.attempt).unwrap().is_none());
    });
}
