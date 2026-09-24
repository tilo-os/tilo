use super::*;
use crate::host::{ActionRuntime, CheckpointCadence, LocalHost, refresh_actions};
use std::time::Duration;

// Removing the production pre-mount checkpoint leaves the old generation and
// old signed publication in place, regardless of the independent worker gate.
#[tokio::test(flavor = "current_thread")]
async fn mount_refresh_commits_before_normal_worker_factory() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let original_generation = owner.committed_generation();
    let original_document = owner.peer().document("dev.qos.chat", "").unwrap();
    let original_publication = owner
        .outgoing()
        .iter()
        .map(|entry| entry.bytes.to_vec())
        .collect::<Vec<_>>();
    let committed = CommittedAuthority::new(&owner);
    let mut runtime = ActionRuntime::new(committed.clone());
    let host = LocalHost::start(&owner).await.unwrap();
    let mut connected = host.connect(&owner).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !connected.is_ready() {
            connected
                .recv()
                .await
                .expect("actual live native subscription");
        }
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
    let generation = owner.committed_generation();
    let publication_changed = owner
        .outgoing()
        .iter()
        .map(|entry| entry.bytes.to_vec())
        .collect::<Vec<_>>()
        != original_publication;
    let published_generation = committed.read().unwrap().generation();
    let document = owner.peer().document("dev.qos.chat", "").unwrap();
    assert!(action_error.is_none(), "{action_error:?}");
    assert!(runtime.actions.is_some());
    assert!(
        cadence.pending && cadence.replacement_pending,
        "new mount checkpoint must enter the existing publication cadence for an already-Ready session"
    );
    crate::host::publish(&owner, session.as_ref().unwrap()).unwrap();
    cadence.mark_published(std::time::Instant::now());
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
    assert_eq!(
        owner.committed_generation(),
        generation,
        "the same live runtime must not refresh on each pump or reconnect"
    );
    assert!(
        !cadence.pending,
        "unchanged runtime must not re-arm checkpoint publication"
    );
    // Normal required worker installation has its own deliberately closed gate.
    // This test does not install the scoped fixture's authorizer or qualify it.
    if let Some(actions) = runtime.actions.as_mut() {
        let _ = actions.close(&owner);
    }
    drop(runtime);
    session.take().unwrap().close().await;
    host.shutdown().await.unwrap();
    assert!(
        generation > original_generation && publication_changed,
        "factory preparation must first commit a fresh normal governor checkpoint; worker result: {action_error:?}"
    );
    assert_eq!(published_generation, generation);
    assert_eq!(
        document, original_document,
        "refresh changes no app history"
    );
}

#[test]
fn mount_refresh_uses_the_attached_original_pool() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let generation = owner.committed_generation();
    assert!(before_action_mount(&mut owner, &committed, None).is_err());
    assert_eq!(owner.committed_generation(), generation);
    before_action_mount(&mut owner, &committed, Some(&pool)).unwrap();
    assert!(owner.committed_generation() > generation);
    assert_eq!(
        committed.read().unwrap().generation(),
        owner.committed_generation()
    );
}

#[test]
fn mount_refresh_original_origin_retired_after_encoding_never_writes() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let time = committed.read().unwrap().view().policy_time().unwrap();
    let retiring = committed.clone();
    let before = owner.status();
    let path = owner.root().join("session.bin");
    let bytes = std::fs::read(&path).unwrap();
    let thread = std::sync::Arc::new(std::sync::Mutex::new(None));
    let thread_slot = thread.clone();
    let result = crate::owner::policy_fence::with_before_check(
        move || {
            // Real authority retirement invalidates its captured origin before
            // waiting for the already-held owner turn. No clock is substituted.
            *thread_slot.lock().unwrap() = Some(std::thread::spawn(move || retiring.invalidate()));
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while time.now_ns().is_ok() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual origin did not retire"
                );
                std::thread::yield_now();
            }
        },
        || before_action_mount(&mut owner, &committed, None),
    );
    thread.lock().unwrap().take().unwrap().join().unwrap();
    assert!(result.is_err());
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert!(
        owner.durability_healthy(),
        "definite refusal never called Store"
    );
    assert!(matches!(
        committed.observe(),
        crate::committed_authority::Observation::Denied
    ));
}

#[test]
fn mount_refresh_failed_store_never_publishes_a_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let before = owner.status();
    let bytes = std::fs::read(owner.root().join("session.bin")).unwrap();
    let root = owner.root().to_owned();
    let result = crate::store::test_fault::with_fault(
        &root,
        crate::store::test_fault::Point::BeforeWrite,
        || before_action_mount(&mut owner, &committed, None),
    );
    assert!(result.is_err());
    assert_eq!(owner.status(), before);
    assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), bytes);
    assert!(!owner.durability_healthy());
    assert!(matches!(
        committed.observe(),
        crate::committed_authority::Observation::Denied
    ));
}
