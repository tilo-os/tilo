use super::*;
use automerge::transaction::Transactable;
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
#[tokio::test]
async fn native_owner_service_offline_receipt_follows_actual_state_file_barrier() {
    for fail in [false, true] {
        let (directory, owner, room, _, key) =
            super::super::native_workspace_owner::test_enrolled_owner();
        let mut signing = owner
            .with_current(room, |view| Ok(view.peer().clone()))
            .unwrap();
        let mut doc =
            automerge::AutoCommit::load(&signing.document("dev.qos.chat", "").unwrap()).unwrap();
        let before_heads = doc.get_heads();
        doc.put(automerge::ROOT, "native_owner_commit_test", true)
            .unwrap();
        doc.commit();
        let changes = doc
            .get_changes(&before_heads)
            .into_iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        let message = signing
            .signed_mutation(
                "dev.qos.chat",
                "",
                collab_types::DocumentMutation::Incremental(changes),
                &key,
                std::time::SystemTime::now(),
            )
            .unwrap();
        let qos_peer_state::NeighbourMsg::AppDocument {
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
            atomic_file::test_support::with_failure(
                atomic_file::test_support::Checkpoint::BeforePublish,
                || futures_lite::future::block_on(service.commit(request.clone())),
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
            let owner =
                super::super::native_workspace_owner::NativeWorkspaceOwner::open(reopened.shared())
                    .unwrap();
            owner
                .with_current(room, |view| {
                    let bytes = view.peer().document("dev.qos.chat", "").unwrap();
                    let doc = automerge::AutoCommit::load(&bytes).unwrap();
                    use automerge::ReadDoc;
                    assert_eq!(
                        doc.get(automerge::ROOT, "native_owner_commit_test")
                            .unwrap()
                            .unwrap()
                            .0
                            .to_bool(),
                        Some(true)
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}
