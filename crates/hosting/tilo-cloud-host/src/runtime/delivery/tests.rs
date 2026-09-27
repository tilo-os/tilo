use super::*;

#[test]
fn delivery_runtime_tick_uses_actual_selected_payload_owner_and_preserves_legacy() {
    for selected in [false, true] {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        let expected = postcard::to_stdvec(&store.state.outbox[0]).unwrap();
        if selected {
            store
                .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
                .unwrap();
            assert!(store.state.outbox.is_empty());
        }
        let batch = RelayTickBatch::prepare(&mut store).unwrap();
        let before = store.state.generation;
        drop(store);
        let mut offered = vec![];
        let sent = batch
            .publish(|publication| {
                offered.push(postcard::to_stdvec(publication)?);
                if offered.len() > 1 {
                    return Err(tilo_relay::coedit::CoeditError::Disconnected.into());
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(sent, 1, "real selected queue must supply work");
        assert_eq!(offered[0], expected);
        let mut store = Store::open(dir.path(), "agent-admission").unwrap();
        batch.retire(&mut store, sent).unwrap();
        assert_eq!(store.state.generation, before + 1);
        let after = std::fs::read(dir.path().join("state.json")).unwrap();
        batch.retire(&mut store, 0).unwrap();
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), after);
        if selected {
            assert!(store.state.outbox.is_empty());
        }
    }
}

#[test]
fn foreign_authored_item_never_enters_the_outbox_or_reaches_the_relay() {
    for selected in [false, true] {
        let (dir, mut store, member) = crate::agent_owner_tests::enrolled();
        if selected {
            store
                .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
                .unwrap();
        }
        let host = store.key().verifying_key().to_bytes();
        // A member's own signed upload: valid, but not authored by this host.
        let upload =
            crate::durability_tests::document_message(&store, &SigningKey::from_bytes(&[8; 32]));
        let NeighbourMsg::AppDocument { envelope, .. } = &upload else {
            panic!("member upload is not an application document");
        };
        assert_eq!(envelope.author.as_bytes(), &member);
        assert_ne!(envelope.author.as_bytes(), &host);
        assert!(!tilo_workspace_host::host::relay_publishable(
            &host, &upload
        ));

        let persisted = std::fs::read(dir.path().join("state.json")).unwrap();
        let generation = store.state.generation;
        let mut state = store.state.clone();
        state
            .outbox
            .push(StoredPublication::mutation(upload).unwrap());
        let error = store.commit(state).unwrap_err();
        assert!(
            error.to_string().contains("not authored by this host"),
            "{error:#}"
        );
        assert_eq!(store.state.generation, generation);
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            persisted
        );
        drop(store);

        let mut store = Store::open(dir.path(), "agent-admission").unwrap();
        let batch = RelayTickBatch::prepare(&mut store).unwrap();
        let mut offered = 0;
        batch
            .publish(|publication| {
                offered += 1;
                assert!(tilo_workspace_host::host::relay_publishable(
                    &host,
                    &publication.message
                ));
                Ok(())
            })
            .unwrap();
        assert!(offered > 0, "the host's own publications still go out");
    }
}
