use super::*;
use crate::delivery::admission::{
    StateOutcome, StateRefusal,
    tests::{browser_delta, document_text},
};
use qos_relay::delivery_v3::commit::{CommitRef, ConsumerState};

const MACHINE: &str = "agent-admission";

fn selected() -> (tempfile::TempDir, [u8; 32]) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    (dir, device)
}
fn admit(root: &std::path::Path, device: [u8; 32], id: u8, text: &str) -> CommitRef {
    let canonical = browser_delta(root, MACHINE, text);
    let mut store = Store::open(root, MACHINE).unwrap();
    match store
        .admit_delivery_item(device, [id; 16], &canonical, 1024 * 1024)
        .unwrap()
    {
        StateOutcome::Durable(record) => record.committed,
        other => panic!("durable admission required: {other:?}"),
    }
}
fn delivery(store: &Store) -> &DeliveryState {
    store.state.delivery.as_ref().unwrap()
}
fn set_consumer(root: &std::path::Path, device: [u8; 32], acknowledged: Option<CommitRef>) {
    let mut store = Store::open(root, MACHINE).unwrap();
    let mut next = store.state.clone();
    let root_key = next.bound.as_ref().unwrap().root;
    next.delivery.as_mut().unwrap().ledger.consumers.insert(
        device,
        ConsumerState {
            id: [5; 16],
            device,
            root: root_key,
            authorization_view: [6; 32],
            acknowledged,
        },
    );
    store.commit(next).unwrap();
}
fn receipt(
    store: &Store,
    device: [u8; 32],
    id: u8,
) -> Option<&qos_relay::delivery_v3::commit::AdmissionRecord> {
    delivery(store).ledger.admissions.get(&ReceiptKey {
        device,
        delivery: [id; 16],
    })
}

#[test]
fn rotation_certifies_a_snapshot_baseline_and_keeps_admitted_state_and_journals() {
    let (dir, device) = selected();
    let root = dir.path();
    let mut committed = Vec::new();
    for (index, text) in ["alpha ", "beta ", "gamma ", "delta "].iter().enumerate() {
        committed.push(admit(root, device, index as u8 + 1, text));
    }
    // The device acknowledged only the first two commits; the last two are
    // still in its (offline) journal.
    set_consumer(root, device, Some(committed[1].clone()));
    let text_before = document_text(root, MACHINE);
    let mut store = Store::open(root, MACHINE).unwrap();
    let old = delivery(&store).ledger.store_generation;
    assert!(delivery(&store).ledger.replay.len() >= 5);
    let report = store
        .retain_delivery_with(qos_relay::clock::now_ns(), true)
        .unwrap()
        .expect("forced rotation commits");
    let generation = store.state.generation;
    let state = delivery(&store);
    // Snapshot + empty suffix in a fresh incarnation.
    assert_eq!(report.rotated, Some(state.ledger.store_generation));
    assert_ne!(state.ledger.store_generation, old);
    assert_eq!(state.baseline_generation, generation);
    assert!(state.tail.is_empty());
    assert_eq!(state.ledger.replay.len(), 1);
    let first = &state.ledger.replay[&generation];
    assert_eq!(first.previous, None);
    assert_eq!(first.publications, state.baseline);
    assert!(state.ledger.pins.is_empty());
    // Only referenced payloads remain; the pool shrank to the checkpoint.
    assert_eq!(state.pool.len(), referenced(state).len());
    assert!(report.after.pool_bytes <= report.before.pool_bytes);
    assert_eq!(report.after.replay, 1);
    // Registration survives; its old-incarnation cursor does not.
    let consumer = &state.ledger.consumers[&device];
    assert_eq!(consumer.id, [5; 16]);
    assert_eq!(consumer.acknowledged, None);
    // Receipts: released where the sender acknowledged, retained otherwise.
    assert_eq!(report.receipts_released, 2);
    assert!(receipt(&store, device, 1).is_none() && receipt(&store, device, 2).is_none());
    assert_eq!(receipt(&store, device, 4).unwrap().committed, committed[3]);
    drop(store);

    // A full reopen revalidates the rotated aggregate; state is unchanged.
    assert_eq!(document_text(root, MACHINE), text_before);
    // A retained receipt still answers its lost-reply retry, and same ID with
    // another body is still refused.
    let other = browser_delta(root, MACHINE, "changed ");
    let mut store = Store::open(root, MACHINE).unwrap();
    assert!(matches!(
        store.admit_delivery_item(device, [4; 16], &other, 1024 * 1024),
        Err(StateRefusal::Conflict)
    ));
    // New commits form the suffix after the rotated baseline.
    let StateOutcome::Durable(record) = store
        .admit_delivery_item(device, [9; 16], &other, 1024 * 1024)
        .unwrap()
    else {
        panic!("durable admission after rotation")
    };
    assert!(record.committed.sequence > generation);
    assert!(!delivery(&store).tail.is_empty());
    assert_eq!(
        delivery(&store).ledger.replay[&record.committed.sequence].previous,
        Some(CommitRef {
            sequence: generation,
            digest: delivery(&store).ledger.replay[&generation].digest
        })
    );
    drop(store);
    assert!(document_text(root, MACHINE).contains("changed"));
}

#[test]
fn receipt_release_without_rotation_requires_budget_pressure_and_an_acknowledgement() {
    let (dir, device) = selected();
    let root = dir.path();
    let first = admit(root, device, 1, "one ");
    set_consumer(root, device, Some(first));
    let mut store = Store::open(root, MACHINE).unwrap();
    let generation = store.state.generation;
    // Far below the receipt budget: nothing is due, nothing commits.
    assert!(
        store
            .retain_delivery(qos_relay::clock::now_ns())
            .unwrap()
            .is_none()
    );
    assert_eq!(store.state.generation, generation);
    assert!(receipt(&store, device, 1).is_some());
}

#[test]
fn pressure_rotates_at_half_budget_and_live_pins_defer_until_critical() {
    let quiet = Pressure {
        pool_bytes: 1024,
        baseline_bytes: 512,
        roots: 10,
        replay: 10,
        receipts: 10,
        queue: 10,
        live_pins: 0,
    };
    assert!(!quiet.rotation_due() && !quiet.receipts_due());
    let pool = Pressure {
        pool_bytes: schema::POOL_LIMIT / 2,
        ..quiet
    };
    assert!(pool.rotation_due());
    // History beyond the baseline is budgeted far below the pool limit.
    let history = Pressure {
        pool_bytes: 512 + HISTORY_BYTES,
        ..quiet
    };
    assert!(history.rotation_due());
    assert!(
        !Pressure {
            pool_bytes: 512 + HISTORY_BYTES - 1,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        !Pressure {
            live_pins: 1,
            ..history
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            live_pins: 1,
            pool_bytes: 512 + (HISTORY_BYTES * 18).div_ceil(10),
            ..quiet
        }
        .rotation_due()
    );
    // A large baseline raises the history budget to its own size.
    assert!(
        !Pressure {
            pool_bytes: 8 * HISTORY_BYTES + HISTORY_BYTES,
            baseline_bytes: 8 * HISTORY_BYTES,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            replay: HISTORY_COMMITS,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        !Pressure {
            replay: HISTORY_COMMITS - 1,
            ..quiet
        }
        .rotation_due()
    );
    // A pool that is mostly the baseline itself does not rotate repeatedly.
    assert!(
        !Pressure {
            pool_bytes: schema::POOL_LIMIT * 3 / 4,
            baseline_bytes: schema::POOL_LIMIT / 2,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            roots: MAX_REFERENCES / 2,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            replay: MAX_REFERENCES / 2,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            queue: MAX_REFERENCES / 2,
            ..quiet
        }
        .rotation_due()
    );
    assert!(
        Pressure {
            receipts: MAX_ADMISSIONS / 2,
            ..quiet
        }
        .receipts_due()
    );
}

#[test]
fn nothing_due_commits_nothing() {
    let (dir, device) = selected();
    admit(dir.path(), device, 1, "small ");
    let mut store = Store::open(dir.path(), MACHINE).unwrap();
    let generation = store.state.generation;
    assert!(
        store
            .retain_delivery(qos_relay::clock::now_ns())
            .unwrap()
            .is_none()
    );
    assert_eq!(store.state.generation, generation);
}
