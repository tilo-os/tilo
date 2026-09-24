use super::*;

#[test]
fn delivery_relay_exact_original_backing_survives_store_and_send_retirement() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let delivery = store.state.delivery.as_ref().unwrap();
    let pointers = delivery
        .queue
        .iter()
        .take(16)
        .map(|entry| delivery.pool.resolve(&entry.payload).unwrap().as_ptr())
        .collect::<Vec<_>>();
    let replay = delivery.ledger.replay.clone();
    let consumers = delivery.ledger.consumers.clone();
    let pool = delivery
        .pool
        .encode(super::super::schema::POOL_LIMIT)
        .unwrap();
    let batch = store.delivery_relay_batch().unwrap();
    assert!(!batch.entries.is_empty());
    assert!(batch.entries.len() <= 16);
    assert_eq!(
        batch
            .entries
            .iter()
            .map(|(_, payload)| payload.bytes().as_ptr())
            .collect::<Vec<_>>(),
        pointers
    );
    let bytes = batch
        .messages()
        .map(|message| postcard::to_stdvec(&message.unwrap()).unwrap())
        .collect::<Vec<_>>();
    drop(store);
    assert_eq!(
        batch
            .messages()
            .map(|message| postcard::to_stdvec(&message.unwrap()).unwrap())
            .collect::<Vec<_>>(),
        bytes
    );
    let mut current = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(current.retire_delivery_relay_batch(&batch, 1).unwrap());
    let delivery = current.state.delivery.as_ref().unwrap();
    assert_eq!(delivery.ledger.replay, replay);
    assert_eq!(
        delivery.ledger.consumers, consumers,
        "relay send is not device ACK"
    );
    assert_eq!(
        delivery
            .pool
            .encode(super::super::schema::POOL_LIMIT)
            .unwrap(),
        pool
    );
    assert!(
        !current.retire_delivery_relay_batch(&batch, 1).unwrap(),
        "stale prefix must not retire next entry"
    );
}

#[test]
fn delivery_relay_zero_pressure_and_wrong_incarnation_never_retire() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let mut batch = store.delivery_relay_batch().unwrap();
    let path = dir.path().join("state.json");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!store.retire_delivery_relay_batch(&batch, 0).unwrap());
    assert!(
        store
            .retire_delivery_relay_batch(&batch, batch.entries.len() + 1)
            .is_err()
    );
    batch.incarnation[0] ^= 1;
    assert!(!store.retire_delivery_relay_batch(&batch, 1).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
