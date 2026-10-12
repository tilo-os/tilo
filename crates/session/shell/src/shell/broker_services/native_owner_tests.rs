use super::*;
use automerge::ReadDoc as _;
use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
#[allow(
    clippy::too_many_lines,
    reason = "one owner-service scenario run with and without an injected state-file failure; \
              both passes assert on the same durable barrier"
)]
#[tokio::test]
async fn native_owner_service_offline_receipt_follows_actual_state_file_barrier() {
    for fail in [false, true] {
        let (directory, owner, room, _, key) =
            super::super::native_workspace_owner::test_enrolled_owner();
        let mut signing = owner
            .with_current(room, |view| Ok(view.peer().clone()))
            .unwrap();
        let mut doc =
            automerge::AutoCommit::load(&signing.document("so.tilo.chat", "").unwrap()).unwrap();
        let before_heads = doc.get_heads();
        doc.put(automerge::ROOT, "native_owner_commit_test", true)
            .unwrap();
        doc.commit_edit("fixture", "native_owner_commit_test");
        let changes = doc
            .get_changes(&before_heads)
            .into_iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        let message = signing
            .signed_mutation(
                "so.tilo.chat",
                "",
                collab_types::DocumentMutation::Incremental(changes),
                &key,
                std::time::SystemTime::now(),
            )
            .unwrap();
        let tilo_peer_state::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            panic!("signed native document")
        };
        let path = directory.path().join("state.json");
        drop(owner);
        let mut tree = super::super::persist::load_tree_from(&path).unwrap();
        tree.persist_path = Some(path.clone());
        let service = NativeOwnerCommitService::new(tree.shared(), None);
        assert!(OwnerCommitService::requires_room(
            &service,
            &room.to_string()
        ));
        assert!(!OwnerCommitService::requires_room(
            &service,
            &uuid::Uuid::new_v4().to_string()
        ));
        let request = OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::Document {
                app: app_id,
                record: record_id,
                envelope,
            },
        };
        let before = std::fs::read(&path).unwrap();
        let result = if fail {
            // The adapter's future has no await; poll it inside the real storage failure scope.
            tilo_files::native::atomic::test_support::with_failure(
                tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
                || {
                    let mut future = std::pin::pin!(service.commit(request.clone()));
                    match future
                        .as_mut()
                        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    {
                        std::task::Poll::Ready(result) => result,
                        std::task::Poll::Pending => {
                            panic!("the adapter's commit future completes without awaiting")
                        }
                    }
                },
            )
        } else {
            service.commit(request.clone()).await
        };
        if fail {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        } else {
            result.unwrap().validate_for(&request).unwrap();
            assert_ne!(std::fs::read(&path).unwrap(), before);
            drop(service);
            let mut reopened = super::super::persist::load_tree_from(&path).unwrap();
            reopened.persist_path = Some(path);
            let owner = super::super::native_workspace_owner::open(reopened.shared()).unwrap();
            owner
                .with_current(room, |view| {
                    let bytes = view.peer().document("so.tilo.chat", "").unwrap();
                    let doc = automerge::AutoCommit::load(&bytes).unwrap();
                    assert_eq!(
                        doc.get(automerge::ROOT, "native_owner_commit_test")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_bool(),
                        Some(true)
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}
