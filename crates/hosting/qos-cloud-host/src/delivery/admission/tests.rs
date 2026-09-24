use crate::{Store, now_time};
use ed25519_dalek::SigningKey;
use qos_actions::Action;

const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "tab:0000000000000001";

/// Canonical browser-authored incremental delta from the enrolled [8; 32] device.
pub(crate) fn browser_delta(root: &std::path::Path, machine: &str, text: &str) -> Vec<u8> {
    let store = Store::open(root, machine).unwrap();
    let now = qos_relay::clock::now_ns();
    let mut browser = store.peer_at(now).unwrap();
    drop(store);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut doc = automerge::AutoCommit::load(&browser.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    let heads = doc.get_heads();
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: text.into(),
    }
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    let message = browser
        .publish_incremental(APP, RECORD, doc.save_after(&heads), &device, now_time(now))
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}

pub(crate) fn document_text(root: &std::path::Path, machine: &str) -> String {
    let store = Store::open(root, machine).unwrap();
    let peer = store.peer_at(qos_relay::clock::now_ns()).unwrap();
    let doc = automerge::AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    qdoc::model::blocks_in_order(&doc, &model)
        .iter()
        .filter_map(|id| qdoc::model::block_text(&doc, &model, id))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cloud_admission_binds_receipt_in_same_commit_and_survives_reopen() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let canonical = browser_delta(dir.path(), "agent-admission", "durable");
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let before = store.state.generation;
    let outcome = store
        .admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
        .unwrap();
    let super::StateOutcome::Durable(record) = outcome else {
        panic!("durable admission required")
    };
    assert_eq!(store.state.generation, before + 1, "one atomic commit");
    assert!(store.state.delivery_binding.is_none());
    drop(store);
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    let key = qos_relay::delivery_v3::commit::ReceiptKey {
        device,
        delivery: [7; 16],
    };
    assert_eq!(ledger.admissions[&key], record);
    assert_eq!(record.committed.sequence, store.state.generation);
    drop(store);
    assert!(document_text(dir.path(), "agent-admission").contains("durable"));
}

// ---- Lane D2: one Store commit and replay group per multi-item request.

/// Consecutive browser deltas, each building on the previous one.
pub(crate) fn browser_deltas(
    root: &std::path::Path,
    machine: &str,
    texts: &[&str],
) -> Vec<Vec<u8>> {
    let store = Store::open(root, machine).unwrap();
    let now = qos_relay::clock::now_ns();
    let mut browser = store.peer_at(now).unwrap();
    drop(store);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut doc = automerge::AutoCommit::load(&browser.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    texts
        .iter()
        .map(|text| {
            let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
            let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
            let heads = doc.get_heads();
            word_processor::actions::WordProcessorAction::SpliceText {
                region: word_processor::actions::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: (*text).into(),
            }
            .apply(&mut doc)
            .unwrap();
            doc.commit();
            let message = browser
                .publish_incremental(APP, RECORD, doc.save_after(&heads), &device, now_time(now))
                .unwrap();
            postcard::to_stdvec(&message).unwrap()
        })
        .collect()
}

#[test]
fn cloud_group_binds_every_receipt_to_one_commit_and_replay_group() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let deltas = browser_deltas(dir.path(), "agent-admission", &["one ", "two ", "three "]);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let before = store.state.generation;
    let replay_before = store.state.delivery.as_ref().unwrap().ledger().replay.len();
    let items = deltas
        .iter()
        .enumerate()
        .map(|(i, delta)| ([i as u8 + 1; 16], delta.as_slice()))
        .collect::<Vec<_>>();
    let records = store
        .admit_delivery_group(device, &items, 1024 * 1024)
        .unwrap()
        .expect("three new document items group");
    assert_eq!(records.len(), 3);
    assert_eq!(store.state.generation, before + 1, "one atomic commit");
    assert!(store.state.delivery_binding.is_none());
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    assert_eq!(ledger.replay.len(), replay_before + 1, "one replay group");
    let group = records[0].committed.clone();
    assert_eq!(group.sequence, store.state.generation);
    let entry = &ledger.replay[&group.sequence];
    for record in &records {
        assert_eq!(record.committed, group);
        assert!(entry.admitted_operations.contains(&record.operations[0]));
    }
    drop(store);
    // Receipts survive reopen; a lost-reply retry answers from them alone.
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    for ((delivery, delta), record) in items.iter().zip(&records) {
        let key = qos_relay::delivery_v3::commit::ReceiptKey {
            device,
            delivery: *delivery,
        };
        assert_eq!(
            &store.state.delivery.as_ref().unwrap().ledger().admissions[&key],
            record
        );
        assert_eq!(
            store
                .delivery_receipt(device, *delivery, *blake3::hash(delta).as_bytes())
                .unwrap()
                .as_ref(),
            Some(record)
        );
    }
    assert!(
        store
            .admit_delivery_group(device, &items, 1024 * 1024)
            .unwrap()
            .is_none()
    );
    drop(store);
    let text = document_text(dir.path(), "agent-admission");
    assert!(text.contains("three two one"), "{text}");
}

#[test]
fn cloud_group_stops_before_a_refused_item_and_never_commits_it() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let deltas = browser_deltas(dir.path(), "agent-admission", &["a ", "b "]);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let before = store.state.generation;
    let garbage = b"not canonical".as_slice();
    // A refused item first: nothing groups, the caller takes the single path.
    assert!(
        store
            .admit_delivery_group(
                device,
                &[([9; 16], garbage), ([1; 16], &deltas[0])],
                1024 * 1024
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(store.state.generation, before);
    // Two new items then a refused one: the pair commits, the refusal does not.
    let records = store
        .admit_delivery_group(
            device,
            &[
                ([1; 16], &deltas[0]),
                ([2; 16], &deltas[1]),
                ([9; 16], garbage),
            ],
            1024 * 1024,
        )
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(store.state.generation, before + 1);
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    assert!(
        !ledger
            .admissions
            .contains_key(&qos_relay::delivery_v3::commit::ReceiptKey {
                device,
                delivery: [9; 16],
            })
    );
}

#[test]
fn cloud_group_forwards_each_verified_item_before_its_one_barrier() {
    use std::sync::{Arc, Mutex};
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let deltas = browser_deltas(dir.path(), "agent-admission", &["x ", "y "]);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let root = dir.path().to_owned();
    let disk = move || -> u64 {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("state.json")).unwrap()).unwrap();
        state["generation"].as_u64().unwrap()
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let before = store.state.generation;
    let sink: super::super::early_forward::EarlySink = Arc::new(move |_, bytes| {
        record.lock().unwrap().push((bytes.to_vec(), disk()));
    });
    let items = [
        ([1; 16], deltas[0].as_slice()),
        ([2; 16], deltas[1].as_slice()),
    ];
    super::super::early_forward::scoped(Some(&sink), || {
        store.admit_delivery_group(device, &items, 1024 * 1024)
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(deltas[0].clone(), before), (deltas[1].clone(), before)],
        "each item forwarded once, in order, before the durable publication"
    );
}
