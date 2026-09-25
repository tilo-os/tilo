use super::*;
use crate::https_owner::{CloudStoreSource, Current};
use qos_peer_state::owner_commit::OwnerCommitService;
use std::os::unix::fs::PermissionsExt;

fn owner() -> (
    tempfile::TempDir,
    Arc<RuntimeOwner>,
    CloudStoreSource,
    PolicyTime,
) {
    let (dir, store, _) = crate::actions::media_tests::voice_store();
    let clock = OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("current source");
    };
    let owner = Arc::new(
        RuntimeOwner::open_with_source(
            dir.path(),
            "agent-admission",
            clock,
            time.clone(),
            source.clone(),
        )
        .unwrap(),
    );
    (dir, owner, source, time)
}
fn replace(dir: &std::path::Path, copy: bool) -> std::path::PathBuf {
    let previous = dir.with_extension("clock-retired-owner");
    std::fs::rename(dir, &previous).unwrap();
    std::fs::create_dir(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if copy {
        for name in ["state.json", "state.lock"] {
            std::fs::copy(previous.join(name), dir.join(name)).unwrap();
        }
    }
    previous
}
#[tokio::test]
async fn cloud_policy_clock_actual_queued_document_rejects_replacement_store() {
    let (dir, owner, source, time) = owner();
    let (route, certificate, mutation) = {
        let store = owner.store().unwrap();
        let peer = store.peer().unwrap();
        (
            ui::collaboration::RouteSpec::app("so.tilo.chat", peer.room().parse().unwrap()),
            peer.authority().clone(),
            collab_types::DocumentMutation::Snapshot(peer.document("so.tilo.chat", "").unwrap()),
        )
    };
    let (documents, mut requests) =
        qos_action_broker::host_documents::HostDocuments::new(Arc::new(owner.ordinary()));
    let signer = documents.bind(route.clone(), certificate);
    let task = tokio::spawn(signer.commit(&route, mutation));
    let pending = requests.recv().await.unwrap();
    let previous = replace(dir.path(), true);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let (complete, result) = pending.apply(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        |route, cert, mutation| owner.store()?.commit_agent_document(route, cert, mutation),
    );
    complete();
    let reply = task.await.unwrap();
    let after = std::fs::read(dir.path().join("state.json")).unwrap();
    std::fs::remove_dir_all(previous).unwrap();
    assert!(
        result.is_err() && reply.is_err(),
        "old runtime committed through replacement directory"
    );
    assert_eq!(after, before);
    assert!(time.now_ns().is_err());
    assert!(matches!(source.with_current(|_| ()), Current::Denied));
    assert!(owner.ordinary().health().is_err());
}
#[test]
fn cloud_policy_clock_selected_backend_never_provisions_missing_replacement() {
    let (dir, owner, _source, time) = owner();
    let previous = replace(dir.path(), false);
    let result = owner.store();
    let created = dir.path().join("state.json").exists();
    std::fs::remove_dir_all(previous).unwrap();
    assert!(
        result.is_err(),
        "selected backend accepted missing replacement"
    );
    assert!(
        !created,
        "selected backend provisioned an unbound replacement Store"
    );
    assert!(time.now_ns().is_err());
}

#[tokio::test]
async fn cloud_policy_clock_ordinary_broker_rejects_replacement_store() {
    let (dir, owner, _source, time) = owner();
    let (certificate, action) = {
        let store = owner.store().unwrap();
        let peer = store.peer().unwrap();
        let certificate = peer.authority().clone();
        let action = qos_relay::peer_actions::DirectedAction {
            room: peer.room().parse().unwrap(),
            request_id: uuid::Uuid::new_v4(),
            source: certificate.device_public_key,
            target: certificate.device_public_key,
            message: qos_relay::peer_actions::ActionMessage::CatalogueRefresh { scope: None },
        };
        (certificate, action)
    };
    let previous = replace(dir.path(), true);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let result = crate::broker::handle_with_policy_time(
        dir.path(),
        "agent-admission",
        &certificate,
        &action,
        &time,
        owner.policy_context(),
    )
    .await;
    let after = std::fs::read(dir.path().join("state.json")).unwrap();
    std::fs::remove_dir_all(previous).unwrap();
    assert!(
        !matches!(
            result,
            Ok(qos_relay::peer_actions::ActionMessage::BrokerCatalogue { .. })
        ),
        "ordinary fallback disclosed from replacement Store"
    );
    assert_eq!(after, before);
    assert!(time.now_ns().is_err());
}

#[tokio::test]
async fn cloud_policy_clock_selected_source_retirement_denies_pending_document_after_valid_commit()
{
    let (dir, owner, source, time) = owner();
    let (route, certificate, mutation) = {
        let store = owner.store().unwrap();
        let peer = store.peer().unwrap();
        (
            ui::collaboration::RouteSpec::app("so.tilo.chat", peer.room().parse().unwrap()),
            peer.authority().clone(),
            collab_types::DocumentMutation::Snapshot(peer.document("so.tilo.chat", "").unwrap()),
        )
    };
    let (documents, mut requests) =
        qos_action_broker::host_documents::HostDocuments::new(Arc::new(owner.ordinary()));
    let signer = documents.bind(route.clone(), certificate);
    let first = tokio::spawn(signer.commit(&route, mutation.clone()));
    let pending = requests.recv().await.unwrap();
    let (complete, result) = pending.apply(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        |route, cert, mutation| owner.store()?.commit_agent_document(route, cert, mutation),
    );
    assert!(result.is_ok(), "valid selected owner document failed");
    complete();
    assert!(first.await.unwrap().is_ok());

    let second = tokio::spawn(signer.commit(&route, mutation));
    let pending = requests.recv().await.unwrap();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    source.retire();
    let (complete, result) = pending.apply(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        |route, cert, mutation| owner.store()?.commit_agent_document(route, cert, mutation),
    );
    complete();
    assert!(result.is_err());
    assert!(second.await.unwrap().is_err());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    assert!(time.now_ns().is_err());
    assert!(owner.policy_time().is_err());
}
