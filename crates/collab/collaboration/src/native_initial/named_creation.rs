//! A native construction holding a named record's creation capture: what
//! the record store's tests open the real canonical record path against
//! (`ui::record`), not a permissive validator.
use super::{
    BridgeState, CONSTRUCTION, CollaborationManager, CommittedDocument, Construction,
    InitialReadBridge, NativeConstruction, QueuedSdkCommand, RouteSpec, SdkCommand,
};
use crate::broker::{ByteBudget, committed_document::read_capture_using};
use crate::record::{RecordCheckout, RecordId, RecordKind};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use runtime_collab::committed_document::{
    CreationChunk, CreationReceipt, CreationWorkspace, Reply,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

/// A native construction holding the creation capture of one named record
/// (`store.catalog` in a new room), as the record store opens it.
pub struct Fixture {
    construction: NativeConstruction,
    _sender: tokio::sync::mpsc::Sender<QueuedSdkCommand>,
    /// The capture.
    pub read: CommittedDocument,
    /// The room the capture names.
    pub room: String,
    /// The record it is of.
    pub checkout: RecordCheckout,
    receiver: Option<tokio::sync::mpsc::Receiver<QueuedSdkCommand>>,
}

/// The construction with a capture that has (`receipt`) or lacks its creation receipt, for
/// this room or another (`wrong_room`), holding an unknown field.
pub async fn fixture(receipt: bool, wrong_room: bool) -> Fixture {
    fixture_graph(receipt, wrong_room, true).await
}

/// [`fixture`], with the unknown field only when `with_unknown`.
pub async fn fixture_graph(receipt: bool, wrong_room: bool, with_unknown: bool) -> Fixture {
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
        construction: NativeConstruction { state, previous },
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
        let command =
            tilo_test_env::wait_for("Initialize to be queued", || receiver.try_recv().ok());
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

impl Fixture {
    /// Whether nothing was asked of the owner yet.
    pub fn nothing_queued(&mut self) -> bool {
        self.receiver
            .as_mut()
            .is_some_and(|receiver| receiver.try_recv().is_err())
    }

    /// Answer the `Initialize` the record store sends, accepting it or not.
    ///
    /// # Panics
    /// When answered twice, or the store sends anything else.
    pub fn acknowledge_initialization(&mut self, accept: bool) -> std::thread::JoinHandle<()> {
        acknowledge_initialization(self.receiver.take().expect("answered once"), accept)
    }

    /// The owner confirms the capture was initialized.
    pub fn initialized(&self) {
        self.read.initialized();
    }

    /// The owner hands this device the record's history again without a
    /// receipt: the same graph, or one with new history when `changed`.
    pub async fn refresh(&self, changed: bool) -> CommittedDocument {
        let bridge = self.construction.state.bridge.as_ref().unwrap();
        let key = RouteSpec::record(
            "so.tilo.store",
            bridge.route.cluster_id,
            self.checkout.id.as_str(),
        )
        .channel_id();
        let read = receiptless_refresh(&self.read, changed).await;
        bridge
            .state
            .document_reads
            .refresh_for_test(&key, read.clone());
        read
    }
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
