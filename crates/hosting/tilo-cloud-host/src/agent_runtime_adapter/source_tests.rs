use super::*;
use crate::https_owner::{CloudStoreSource, Current};

fn owner() -> (
    tempfile::TempDir,
    Arc<RuntimeOwner>,
    CloudStoreSource,
    PolicyTime,
) {
    let (dir, store, _) = crate::actions::media_tests::voice_store();
    let clock = OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
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
        tilo_action_broker::host_documents::HostDocuments::new(Arc::new(owner.ordinary()));
    let signer = documents.bind(route.clone(), certificate);
    let first = tokio::spawn(signer.commit(&route, mutation.clone()));
    let pending = requests.recv().await.unwrap();
    let (complete, result) = pending
        .apply(tilo_test_env::hang_deadline(), |route, cert, mutation| {
            owner.store()?.commit_agent_document(route, cert, mutation)
        });
    assert!(result.is_ok(), "valid selected owner document failed");
    complete();
    assert!(first.await.unwrap().is_ok());

    let second = tokio::spawn(signer.commit(&route, mutation));
    let pending = requests.recv().await.unwrap();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    source.retire();
    let (complete, result) = pending
        .apply(tilo_test_env::hang_deadline(), |route, cert, mutation| {
            owner.store()?.commit_agent_document(route, cert, mutation)
        });
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
