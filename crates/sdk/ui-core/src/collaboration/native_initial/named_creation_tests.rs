//! Exercise the real canonical record opening path, not a permissive validator.
use super::{
    BridgeState, CONSTRUCTION, CollaborationManager, CommittedDocument, Construction,
    InitialReadBridge, NativeConstruction, QueuedSdkCommand, RouteSpec, SdkCommand,
    take_initialization,
};
use crate::collaboration::broker::{ByteBudget, committed_document::read_capture_using};
use crate::record::{RecordCheckout, RecordId, RecordKind, RecordOwners};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use runtime_collab::committed_document::{
    CreationChunk, CreationReceipt, CreationWorkspace, Reply,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

struct Fixture {
    _construction: NativeConstruction,
    _sender: tokio::sync::mpsc::Sender<QueuedSdkCommand>,
    read: CommittedDocument,
    room: String,
    checkout: RecordCheckout,
    receiver: Option<tokio::sync::mpsc::Receiver<QueuedSdkCommand>>,
}

async fn fixture(receipt: bool, wrong_room: bool) -> Fixture {
    fixture_graph(receipt, wrong_room, true).await
}

async fn fixture_graph(receipt: bool, wrong_room: bool, with_unknown: bool) -> Fixture {
    let room_id = uuid::Uuid::new_v4();
    let room = room_id.to_string();
    let checkout = RecordCheckout::new(RecordId::room_default(), RecordKind::new("store.catalog"));
    let mut document = AutoCommit::new();
    if with_unknown {
        document
            .put(ROOT, "future_owner_field", "preserved")
            .unwrap();
        document.commit();
    }
    let bytes = document.save();
    let chunk = CreationChunk {
        capture: 1,
        revision: [3; 32],
        total: u32::try_from(bytes.len()).unwrap(),
        bytes,
        token: [4; 16],
    };
    let first = if receipt {
        Reply::Created {
            receipt: CreationReceipt {
                token: [7; 16],
                workspace: CreationWorkspace {
                    id: if wrong_room {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        room.clone()
                    },
                    name: "Actual owner room".into(),
                },
            },
            chunk,
        }
    } else {
        Reply::Chunk {
            capture: chunk.capture,
            revision: chunk.revision,
            total: chunk.total,
            offset: 0,
            bytes: chunk.bytes,
            token: chunk.token,
        }
    };
    let (read, _) = read_capture_using(first, &ByteBudget::new(), async |_| Ok(Reply::Cancelled))
        .await
        .unwrap();
    let bridge_state = BridgeState::new();
    let route = RouteSpec::record("so.tilo.store", room_id, checkout.id.as_str());
    bridge_state
        .document_reads
        .stage_for_test(&route.channel_id(), read.clone());
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let bridge = InitialReadBridge {
        route: RouteSpec::app("so.tilo.store", room_id),
        commands: sender.downgrade(),
        state: bridge_state,
    };
    let state = Rc::new(Construction {
        manager: RefCell::new(Some(CollaborationManager::disabled())),
        bridge: Some(bridge),
        primary: RefCell::new(None),
        creation: RefCell::new(BTreeMap::new()),
        failure: RefCell::new(None),
        transferring: Cell::new(false),
        component_scope: RefCell::new(None),
    });
    let previous = CONSTRUCTION.with(|slot| slot.replace(Some(Rc::downgrade(&state))));
    Fixture {
        _construction: NativeConstruction { state, previous },
        _sender: sender,
        read,
        room,
        checkout,
        receiver: Some(receiver),
    }
}

fn acknowledge_initialization(
    mut receiver: tokio::sync::mpsc::Receiver<QueuedSdkCommand>,
    accept: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let command = loop {
            if let Ok(command) = receiver.try_recv() {
                break command;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Initialize was not queued"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let SdkCommand::Initialize { request, reply, .. } = command.command else {
            panic!("expected the actual Initialize command");
        };
        let runtime_collab::committed_document::Request::Initialize { document, kind, .. } =
            request
        else {
            panic!("expected a receipt-bound Initialize request");
        };
        assert_eq!(kind, "store.catalog");
        let document = AutoCommit::load(&document).unwrap();
        assert_eq!(
            document
                .get(ROOT, "initialized")
                .unwrap()
                .unwrap()
                .0
                .as_bool(),
            Some(true)
        );
        reply
            .send(if accept {
                Ok([6; 32])
            } else {
                Err("owner refused initialization".into())
            })
            .unwrap();
    })
}

#[tokio::test(flavor = "current_thread")]
async fn named_genesis_install_requires_ack_and_cannot_replace_late_changes() {
    for (accept, late_change) in [(true, false), (false, false), (true, true)] {
        let mut fixture = fixture(true, false).await;
        let directory = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(directory.path().to_owned());
        let store = owners.open("so.tilo.store", &fixture.room).unwrap();
        store.try_open_record(&fixture.checkout).unwrap();
        let initialization = take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .unwrap();
        assert_original_scope(&fixture, &store, &initialization);
        assert!(fixture.receiver.as_mut().unwrap().try_recv().is_err());
        let ack = acknowledge_initialization(fixture.receiver.take().unwrap(), accept);
        let result = store.initialize_admitted_record(
            &fixture.checkout,
            &initialization,
            |doc| {
                doc.put(ROOT, "initialized", true).unwrap();
                Ok(())
            },
            |_| {
                assert!(
                    store.route(&fixture.checkout).is_err(),
                    "never Ready before ACK"
                );
                if late_change {
                    store
                        .with(&fixture.checkout.id, |doc| {
                            doc.put(ROOT, "late_local_change", true).unwrap();
                            doc.commit();
                        })
                        .unwrap();
                }
                Ok(())
            },
        );
        ack.join().unwrap();
        assert_eq!(result.is_ok(), accept && !late_change);
        assert_eq!(
            store.route(&fixture.checkout).is_ok(),
            accept && !late_change
        );
        store
            .read_checked(&fixture.checkout.id, |doc| {
                assert_eq!(
                    doc.get(ROOT, "initialized").unwrap().is_some(),
                    accept && !late_change
                );
                assert_eq!(
                    doc.get(ROOT, "late_local_change").unwrap().is_some(),
                    late_change
                );
                assert_eq!(
                    doc.get(ROOT, "future_owner_field")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("preserved")
                );
            })
            .unwrap();
        if accept {
            assert!(initialization.workspace().is_none());
            assert!(
                store
                    .initialize_admitted_record(
                        &fixture.checkout,
                        &initialization,
                        |_| Ok(()),
                        |_| Ok(())
                    )
                    .is_err()
            );
        }
        assert!(
            take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
                .unwrap()
                .is_none()
        );
    }
}

fn assert_original_scope(
    fixture: &Fixture,
    store: &crate::record::RecordStore,
    initialization: &super::RecordInitialization,
) {
    let original = store
        .read_checked(&fixture.checkout.id, Clone::clone)
        .unwrap();
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("wrong.kind"));
    let wrong_record = RecordCheckout::new(RecordId::minted(), fixture.checkout.kind.clone());
    for checkout in [&wrong_kind, &wrong_record] {
        assert!(
            initialization
                .validate_original(&original, checkout, &fixture.room)
                .is_err()
        );
    }
    assert!(
        initialization
            .validate_original(&original, &fixture.checkout, "wrong-room")
            .is_err()
    );
    let mut changed = original;
    changed.put(ROOT, "changed_heads", true).unwrap();
    changed.commit();
    assert!(
        initialization
            .validate_original(&changed, &fixture.checkout, &fixture.room)
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_empty_creation_admits_before_initialization() {
    let fixture = fixture_graph(true, false, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    assert!(store.admission_route(&fixture.checkout).is_ok());
    assert!(store.route(&fixture.checkout).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_creation_admits_exact_graph_but_not_ready_identity() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    assert!(
        store.route(&fixture.checkout).is_err(),
        "not Ready before Initialize"
    );
    let admitted = store.admission_route(&fixture.checkout).unwrap();
    assert_eq!(admitted.app_id, "so.tilo.store");
    assert_eq!(
        admitted.record.as_deref(),
        Some(fixture.checkout.id.as_str())
    );
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("wrong.kind"));
    assert!(store.admission_route(&wrong_kind).is_err());
    store
        .read(&fixture.checkout.id, |doc| {
            assert_eq!(
                doc.get(ROOT, "future_owner_field")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("preserved")
            );
        })
        .unwrap();
    assert!(
        take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .is_some()
    );
    fixture.read.initialized();
    assert!(store.admission_route(&fixture.checkout).is_err());
    assert!(
        store.try_open_record(&fixture.checkout).is_err(),
        "consumed receipt cannot reopen an unfinished graph"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn named_identityless_restore_and_wrong_room_refuse_without_cache_install() {
    for (receipt, wrong_room) in [(false, false), (true, true)] {
        let fixture = fixture(receipt, wrong_room).await;
        let directory = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(directory.path().to_owned());
        let store = owners.open("so.tilo.store", &fixture.room).unwrap();
        assert!(store.try_open_record(&fixture.checkout).is_err());
        assert!(store.open_records().is_empty());
        assert!(store.admission_route(&fixture.checkout).is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn named_creation_does_not_adopt_modified_or_foreign_local_cache() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    store
        .with(&fixture.checkout.id, |doc| {
            doc.put(ROOT, "unadmitted_edit", true).unwrap();
            doc.commit();
        })
        .unwrap();
    assert!(store.try_open_record(&fixture.checkout).is_err());
    assert!(store.admission_route(&fixture.checkout).is_err());
    let other_directory = tempfile::tempdir().unwrap();
    let mut other_owners = RecordOwners::new(other_directory.path().to_owned());
    let foreign = other_owners
        .open_person("so.tilo.store", &fixture.room)
        .unwrap();
    let wrong_kind =
        RecordCheckout::new(fixture.checkout.id.clone(), RecordKind::new("other.kind"));
    foreign.create_record(&wrong_kind).unwrap();
    foreign.flush();
    drop(foreign);
    drop(other_owners);
    let mut restored = RecordOwners::new(other_directory.path().to_owned());
    let restored = restored.open("so.tilo.store", &fixture.room).unwrap();
    assert!(restored.try_open_record(&fixture.checkout).is_err());
    assert!(restored.open_records().is_empty());
    assert!(restored.admission_route(&fixture.checkout).is_err());
}

async fn receiptless_refresh(read: &CommittedDocument, changed: bool) -> CommittedDocument {
    let mut document = read
        .with_mutation(|mutation| {
            let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                panic!("original capture must be a snapshot");
            };
            AutoCommit::load(bytes).unwrap()
        })
        .unwrap();
    if changed {
        document.put(ROOT, "new_owner_history", true).unwrap();
        document.commit();
    }
    let bytes = document.save();
    let reply = Reply::Chunk {
        capture: 2,
        // Even a falsely unchanged revision cannot mask a different graph.
        revision: [3; 32],
        total: u32::try_from(bytes.len()).unwrap(),
        offset: 0,
        bytes,
        token: [9; 16],
    };
    read_capture_using(reply, &ByteBudget::new(), async |_| Ok(Reply::Cancelled))
        .await
        .unwrap()
        .0
}

#[tokio::test(flavor = "current_thread")]
async fn named_original_receipt_survives_same_graph_refresh_but_never_masks_new_history() {
    let fixture = fixture(true, false).await;
    let directory = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(directory.path().to_owned());
    let store = owners.open("so.tilo.store", &fixture.room).unwrap();
    store.try_open_record(&fixture.checkout).unwrap();
    let initialization = take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
        .unwrap()
        .unwrap();
    assert!(
        take_initialization("so.tilo.store", &fixture.room, &fixture.checkout)
            .unwrap()
            .is_none(),
        "construction custody remains one-shot"
    );
    let bridge = fixture._construction.state.bridge.as_ref().unwrap();
    let key = RouteSpec::record(
        "so.tilo.store",
        bridge.route.cluster_id,
        fixture.checkout.id.as_str(),
    )
    .channel_id();
    let same = receiptless_refresh(&fixture.read, false).await;
    assert!(same.creation_workspace().is_none());
    bridge.state.document_reads.refresh_for_test(&key, same);
    assert!(store.admission_route(&fixture.checkout).is_ok());
    assert!(
        store.route(&fixture.checkout).is_err(),
        "not Ready without Initialize ACK"
    );
    assert!(initialization.workspace().is_some());
    let changed = receiptless_refresh(&fixture.read, true).await;
    bridge.state.document_reads.refresh_for_test(&key, changed);
    assert!(store.admission_route(&fixture.checkout).is_err());
    assert!(store.route(&fixture.checkout).is_err());
    fixture.read.initialized();
    assert!(initialization.workspace().is_none());
    assert!(store.admission_route(&fixture.checkout).is_err());
}
