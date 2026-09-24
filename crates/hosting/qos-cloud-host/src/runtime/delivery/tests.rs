use super::*;

#[test]
fn delivery_runtime_tick_uses_actual_selected_payload_owner_and_preserves_legacy() {
    for selected in [false, true] {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        let expected = postcard::to_stdvec(&store.state.outbox[0]).unwrap();
        if selected {
            store
                .activate_delivery_storage(qos_relay::clock::now_ns())
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
                    return Err(qos_relay::coedit::CoeditError::Disconnected.into());
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
