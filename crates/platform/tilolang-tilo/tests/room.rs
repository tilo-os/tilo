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
