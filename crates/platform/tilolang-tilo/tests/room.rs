//! Two windows of one room, each an instance without the toolkit, committing through the
//! collab client: what one window's runtime planned reaches the other's room record and its
//! runtime.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use tilo_render_protocol::patch::local::{End, pair};
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::room::RoomRecord;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::{NameHost, Principal};

use crate::fixtures::{APP, COUNTER};

/// The counter, loaded with the ids the room's first window gave its types.
struct Counter {
    ids: RefCell<String>,
}

impl Platform for Counter {
    fn load(&self, host: &Host) -> u64 {
        let package = host.load_builtin(COUNTER, &self.ids.borrow()).unwrap();
        *self.ids.borrow_mut() = package.ids;
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-10-28T10:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
    }
}

type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// One end of a room's route as the broker relays it: what one replica sends reaches the
/// other as the signed document envelope the broker delivers.
struct Relay {
    route: RouteSpec,
    me: String,
    mine: Inbox,
    theirs: Inbox,
    joined: Option<RouteSpec>,
}

impl PeerTransport for Relay {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match request {
            CollabRequest::Hello { .. } => {
                self.joined = Some(self.route.clone());
            }
            CollabRequest::Document { mutation, .. } => {
                let record = self.route.record.as_deref().unwrap_or("");
                let room = self.route.cluster_id.to_string();
                let mut envelope = collab_types::SignedOp::new_unsigned(
                    mutation.encode().map_err(|error| error.to_string())?,
                    collab_types::document_artifact_id(&self.route.app_id, &room, record),
                    collab_types::Ed25519PublicKey::from_bytes([0; 32]),
                    collab_types::document_attestation_id(&self.route.app_id, &room, record),
                    collab_types::OpKind::Change,
                    std::time::SystemTime::UNIX_EPOCH,
                    0,
                );
                envelope.signature = collab_types::Signature::from_bytes([0; 64]);
                self.theirs.borrow_mut().push_back(CollabEvent::Document {
                    envelope: Box::new(envelope),
                    from_user_id: self.me.clone(),
                    decrypted_payload: None,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.mine.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }
}

/// A window of the counter for `me`, its instance over `doc`, committing on `route`.
struct Window {
    instance: Instance,
    room: RoomRecord,
    _patches: End,
}

fn window(
    me: &str,
    route: &RouteSpec,
    mine: &Inbox,
    theirs: &Inbox,
    platform: &Rc<Counter>,
    doc: automerge::AutoCommit,
) -> Window {
    let mut manager = CollaborationManager::with_peer_transport(Relay {
        route: route.clone(),
        me: me.to_owned(),
        mine: Rc::clone(mine),
        theirs: Rc::clone(theirs),
        joined: None,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.record.clone(),
        record_kind: None,
        handle: Some(me.to_owned()),
    }));
    let _ = manager.poll();
    let (app_end, window_end) = pair();
    let instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: me.to_owned(),
            mode: Mode::Live,
            device: None,
            index_dir: None,
            record_loader: None,
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
        },
        Rc::clone(platform) as Rc<dyn Platform>,
        Rc::new(Alone),
        Box::new(app_end),
    );
    let room = RoomRecord::new(doc, manager, route.clone(), me);
    Window {
        instance,
        room,
        _patches: window_end,
    }
}

#[test]
fn what_one_window_commits_through_the_collab_client_reaches_the_other() {
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let (ada_inbox, grace_inbox): (Inbox, Inbox) = (Rc::default(), Rc::default());
    let platform = Rc::new(Counter {
        ids: RefCell::new(String::new()),
    });
    let mut ada = window(
        "ada",
        &route,
        &ada_inbox,
        &grace_inbox,
        &platform,
        automerge::AutoCommit::new(),
    );
    let mut grace = window(
        "grace",
        &route,
        &grace_inbox,
        &ada_inbox,
        &platform,
        automerge::AutoCommit::new(),
    );

    // Ada's window opens over the empty room: its runtime plans the counter's root, and the
    // room path commits it to her record and sends it.
    let mut committed = Vec::new();
    for _ in 0..100 {
        ada.instance.sync(Some(ada.room.doc()));
        ada.instance.update();
        committed.extend(ada.room.commit(&mut ada.instance).unwrap());
        if !committed.is_empty() {
            break;
        }
    }
    assert!(
        !committed.is_empty(),
        "the root ada's window planned is committed"
    );
    let ada_root = ada.instance.host().unwrap().root_of(APP).unwrap();
    assert!(
        !ada.room.doc().clone().get_changes(&[]).is_empty(),
        "the root is in ada's record"
    );

    // The change reaches grace's record through her collab client, and her window's runtime
    // reads ada's root.
    assert!(
        !grace_inbox.borrow().is_empty(),
        "the room relayed the change"
    );
    assert!(grace.room.receive(&mut grace.instance));
    assert_eq!(
        grace.room.doc().clone().get_heads(),
        ada.room.doc().clone().get_heads(),
        "grace holds what ada committed"
    );
    let grace_root = grace.instance.host().unwrap().root_of(APP).unwrap();
    assert_eq!(
        grace_root, ada_root,
        "grace's window reads the root ada's made"
    );
}

/// The counter's value at `root`, as the instance's runtime reads it.
fn value_at(instance: &Instance, root: &tilolang_tilo::runtime::ResourceRef) -> i64 {
    match instance
        .host()
        .unwrap()
        .runtime()
        .read(root, "value")
        .unwrap()
    {
        tilolang_tilo::runtime::Value::Int(value) => value,
        other => panic!("the counter's value is {other:?}"),
    }
}

/// The room as its host holds it: ada's window made the counter's root. The root, and the room's
/// history.
fn made_by_ada(
    route: &RouteSpec,
    platform: &Rc<Counter>,
) -> (tilolang_tilo::runtime::ResourceRef, Vec<u8>) {
    let (ada_inbox, nobody): (Inbox, Inbox) = (Rc::default(), Rc::default());
    let mut ada = window(
        "ada",
        route,
        &ada_inbox,
        &nobody,
        platform,
        automerge::AutoCommit::new(),
    );
    for _ in 0..100 {
        ada.instance.sync(Some(ada.room.doc()));
        ada.instance.update();
        if !ada.room.commit(&mut ada.instance).unwrap().is_empty() {
            break;
        }
    }
    let root = ada.instance.host().unwrap().root_of(APP).unwrap();
    (root, ada.room.doc().clone().save())
}

/// A room another host governs: the record is the broker's committed read, the window's edit is a
/// step of its own that the person's Undo takes back (and Redo brings back), each sent on the
/// room's route, and the record's on-disk copy keeps up with it.
/// How many versions the record's History lists as `person`'s.
fn steps_of(record: &RoomRecord, person: &str) -> usize {
    let listed = record
        .view_history(br#"{"op":"history","last":null}"#)
        .unwrap()
        .value
        .unwrap();
    listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|version| version["actor"]["id"] == person)
        .count()
}

#[test]
fn a_tilolang_edit_in_a_room_another_host_governs_is_undone() {
    let room_id = uuid::Uuid::new_v4();
    let route = RouteSpec::app(APP, room_id);
    let platform = Rc::new(Counter {
        ids: RefCell::new(String::new()),
    });

    let (root, committed) = made_by_ada(&route, &platform);

    // Grace's window opens on a connection whose route the broker reads committed.
    let (manager, _) =
        CollaborationManager::native_initial_fixture_for_test(APP, room_id, committed);
    let profile = tempfile::tempdir().unwrap();
    let documents: Rc<dyn durable_store::DurableStore> =
        Rc::new(app_state::FsStore::new(profile.path()));
    let mut record = RoomRecord::open(manager, &documents, "grace").unwrap();
    let (app_end, _patches) = pair();
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "grace".to_owned(),
            mode: Mode::Live,
            device: None,
            index_dir: None,
            record_loader: None,
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
        },
        Rc::clone(&platform) as Rc<dyn Platform>,
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync(Some(record.doc()));
    instance.update();
    assert_eq!(
        instance.host().unwrap().root_of(APP).unwrap(),
        root,
        "grace's window reads the root the room's committed read holds"
    );
    assert_eq!(value_at(&instance, &root), 0);
    assert_eq!(record.can_undo(), (false, false), "nothing of hers yet");

    // Grace adds one: the runtime plans it, the room path commits and sends it.
    let host = instance.host().unwrap();
    let increment = host
        .package(APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "increment")
        .unwrap()
        .id
        .clone();
    let first = host.next_ticket();
    host.runtime()
        .invoke_action(
            &tilolang_tilo::runtime::Principal::new("grace"),
            (
                increment.as_str(),
                vec![tilolang_tilo::runtime::Value::resource(&root)],
            ),
        )
        .unwrap();
    let queued = tilolang_tilo::instance::queue_commits(&host, &instance.outgoing(), first, None);
    assert!(!queued.is_empty(), "the edit is queued for the room");
    let sent = record.commit(&mut instance).unwrap();
    assert_eq!(sent, queued);
    instance.update();
    assert_eq!(value_at(&instance, &root), 1);
    assert_eq!(record.can_undo(), (true, false), "her edit is a step");

    // Undo takes it back as a change of its own, and redo brings it back: the history verbs
    // every app's record answers, on the app's resource.
    let resource = tilolang_tilo::publish::package_resource_id(APP);
    assert!(record.undo(&mut instance, &resource, APP).unwrap());
    instance.update();
    assert_eq!(value_at(&instance, &root), 0, "undo took the edit back");
    assert_eq!(record.can_undo(), (false, true));
    assert!(record.redo(&mut instance, &resource, APP).unwrap());
    instance.update();
    assert_eq!(value_at(&instance, &root), 1, "redo brought it back");
    assert_eq!(
        steps_of(&record, "grace"),
        3,
        "history lists her edit, its undo and the redo"
    );

    // The on-disk copy holds the record with the edit, the undo and the redo.
    record.flush();
    assert_eq!(
        record.save_status(),
        collaboration::record::RecordSaveStatus::Saved
    );
    let heads = record.doc().clone().get_heads();
    let mut store =
        collaboration::record::save::room_document(&documents, APP, &room_id.to_string()).unwrap();
    drop(record);
    let mut stored = store
        .load_complete()
        .unwrap()
        .expect("the copy was written");
    assert_eq!(stored.get_heads(), heads);
}
