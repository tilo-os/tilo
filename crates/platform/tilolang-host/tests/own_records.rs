//! Mail is each member's own. Two members of one workspace each run Mail in a tilolang host:
//! each instance keeps its records in that person's own record of the workspace, on a route of
//! its own, so what one member's mail holds never reaches the other's record, views, search or
//! published actions, and the first member's instance refuses what the second asks of it
//! through the door.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test: a failure is a panic"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Lane, ToUi, Transport as _};
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::room::RoomRecord;

const MAIL: &str = "so.tilo.lang.mail";

/// Two members, by the hex of their user root keys.
const ADA: &str = "abababababababababababababababababababababababababababababababab";
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// What only Ada's mail says: in her conversations' list and search, and on the door, which
/// names a conversation by its newest words.
const ADAS: [&str; 3] = ["Launch plan", "Grace Hopper", "lease renewal"];
const ON_HER_DOOR: [&str; 2] = ["Agreed on the twelfth", "The new lease is attached"];

fn compiled(dir: &str, source: &str, ids: &str) -> Bundle {
    let lock = tilolang::IdsLock::parse(ids).unwrap();
    let compiled =
        tilolang::compile_package(source, &lock, &[]).unwrap_or_else(|_| panic!("{dir} compiles"));
    Bundle::new(&compiled, [])
}

fn mail() -> Bundle {
    compiled(
        "mail",
        include_str!("../../../../apps/tl/mail/app.tl"),
        include_str!("../../../../apps/tl/mail/app.ids"),
    )
}

type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;
type Sent = Rc<RefCell<Vec<CollabRequest>>>;

/// The broker, as far as this test needs it: each connection joined one route, and a change
/// sent on a route reaches every other connection joined on that same route, as the record's
/// channel carries it.
#[derive(Default)]
struct Bus {
    joined: RefCell<Vec<(RouteSpec, String, Inbox)>>,
}

struct Connection {
    bus: Rc<Bus>,
    route: RouteSpec,
    me: String,
    inbox: Inbox,
    sent: Sent,
    joined: Option<RouteSpec>,
}

impl PeerTransport for Connection {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match &request {
            CollabRequest::Hello { .. } => {
                self.joined = Some(self.route.clone());
                self.bus.joined.borrow_mut().push((
                    self.route.clone(),
                    self.me.clone(),
                    Rc::clone(&self.inbox),
                ));
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
                for (route, who, inbox) in self.bus.joined.borrow().iter() {
                    if *route == self.route && *who != self.me {
                        inbox.borrow_mut().push_back(CollabEvent::Document {
                            envelope: Box::new(envelope.clone()),
                            from_user_id: self.me.clone(),
                            decrypted_payload: None,
                        });
                    }
                }
            }
            _ => {}
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }
}

/// The window, from the far end of the patch connection: the root's tree as the instance sent
/// it, and everything else it sent (the rows of its lists, the conversation opened beside).
struct Window {
    transport: ChannelTransport,
    root: Option<patch::Node>,
    sent: String,
}

impl Window {
    fn take(&mut self) {
        while let Some(batch) = self.transport.recv().unwrap() {
            for message in patch::decode_batch::<ToUi>(&batch).unwrap() {
                let _ = writeln!(self.sent, "{message:?}");
                match message {
                    ToUi::Open {
                        lane: Lane::Root,
                        tree,
                        ..
                    } => self.root = Some(tree),
                    ToUi::Patch {
                        lane: Lane::Root,
                        patches,
                    } => {
                        let root = self.root.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(root).expect("the patch applies");
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Everything the window was sent to show, as text.
    fn shows(&self) -> String {
        assert!(self.root.is_some(), "the window shows its root");
        self.sent.clone()
    }
}

/// One member running Mail in their tilolang host.
struct Member {
    host: TilolangHost,
    window: Window,
    inbox: Inbox,
    sent: Sent,
    route: RouteSpec,
}

fn wake() -> tilolang_tilo::background::Waker {
    Arc::new(|| {})
}

fn member(bus: &Rc<Bus>, room: uuid::Uuid, person: &str, bundle: &Arc<Bundle>) -> Member {
    let route = tilolang_host::route_of(&bundle.meta, room, person);
    let (inbox, sent): (Inbox, Sent) = (Rc::default(), Rc::default());
    let mut manager = CollaborationManager::with_peer_transport(Connection {
        bus: Rc::clone(bus),
        route: route.clone(),
        me: person.to_owned(),
        inbox: Rc::clone(&inbox),
        sent: Rc::clone(&sent),
        joined: None,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.record.clone(),
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = RoomRecord::new(automerge::AutoCommit::new(), manager, route.clone(), person);
    let mut host = TilolangHost::new(None, wake());
    let kept = host.kept(MAIL).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        Spec {
            app: MAIL.to_owned(),
            room: room.to_string(),
            instance: 1,
            person: person.to_owned(),
        },
        record,
        Arc::clone(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        Rc::new(Phrases::new("en-US")),
        wake(),
    );
    host.add(hosted, Some(watched));
    Member {
        host,
        window: Window {
            transport: ChannelTransport::new(theirs).unwrap(),
            root: None,
            sent: String::new(),
        },
        inbox,
        sent,
        route,
    }
}

impl Member {
    fn hosted(&self) -> &Hosted {
        self.host.instances().next().unwrap()
    }

    /// Frames until nothing changes for a while.
    fn settle(&mut self) {
        let mut quiet: u32 = 0;
        while quiet < 20 {
            let changed = self.host.turn();
            self.window.take();
            if changed {
                quiet = 0;
            } else {
                quiet = quiet.saturating_add(1);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    /// What search finds in the member's Mail: every hit its indexes publish, as text.
    fn found(&self) -> String {
        let host = self.hosted().instance().host().unwrap();
        host.wait_index();
        format!("{:?}", tilolang_tilo::search::every_hit(&host))
    }

    /// What the member's instance publishes on the door (each resource with its actions), as
    /// text.
    fn published(&self) -> String {
        format!("{:?}", self.hosted().published())
    }

    /// The answer the member's instance sent to the door's invocation `id`.
    fn answer(&self, id: u64) -> String {
        self.sent
            .borrow()
            .iter()
            .find_map(|request| match request {
                CollabRequest::UpdateInvocation {
                    id: answered,
                    update,
                } if *answered == id => Some(format!("{update:?}")),
                _ => None,
            })
            .unwrap_or_else(|| panic!("invocation {id} was answered"))
    }
}

/// None of `ada`'s mail reaches `bo`: not his record, his window, his search or his door.
fn nothing_of_reaches(ada: &Member, bo: &Member) {
    assert!(
        bo.inbox.borrow().is_empty(),
        "nothing of Ada's record was delivered to Bo"
    );
    let theirs: std::collections::HashSet<_> = ada
        .hosted()
        .record()
        .doc()
        .clone()
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    assert!(
        bo.hosted()
            .record()
            .doc()
            .clone()
            .get_changes(&[])
            .iter()
            .all(|change| !theirs.contains(&change.hash())),
        "Bo's record holds none of Ada's changes"
    );
    for what in ADAS {
        assert!(
            !bo.window.shows().contains(what),
            "Bo's Mail does not show {what}"
        );
        assert!(
            !bo.found().contains(what),
            "Bo's search does not find {what}"
        );
    }
    for what in ADAS.iter().chain(&ON_HER_DOOR) {
        assert!(
            !bo.published().contains(what),
            "Bo's door does not offer {what}"
        );
    }
}

#[test]
fn mail_is_each_members_own_record_of_the_workspace() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let bundle = Arc::new(mail());
    assert!(bundle.meta.own_records, "Mail declares `records: .own`");
    let room = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, room, ADA, &bundle);
    let mut bo = member(&bus, room, BO, &bundle);

    // Each member's Mail is on their own record of the workspace, which only their devices are
    // admitted to; an app that shares its records stays on the workspace's record of the app.
    assert_eq!(
        ada.route.record.as_deref(),
        Some(format!("member:{ADA}:tilolang.room").as_str())
    );
    assert_eq!(
        bo.route.record.as_deref(),
        Some(format!("member:{BO}:tilolang.room").as_str())
    );
    let counter = compiled(
        "counter",
        include_str!("../../../../apps/tl/counter/app.tl"),
        include_str!("../../../../apps/tl/counter/app.ids"),
    );
    assert_eq!(
        tilolang_host::route_of(&counter.meta, room, ADA),
        RouteSpec::app("so.tilo.lang.counter", room),
        "a shared app's record is the workspace's"
    );
    assert_eq!(
        tilolang_host::route_of(&bundle.meta, room, "ada"),
        RouteSpec::app(MAIL, room),
        "someone without a root identity is a member of no workspace: nobody else reads theirs"
    );

    // Ada's mail arrives: an inbox a week in.
    ada.settle();
    bo.settle();
    ada.host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a busy inbox");
    ada.settle();
    bo.settle();
    for what in ADAS {
        assert!(ada.window.shows().contains(what), "Ada's Mail shows {what}");
        assert!(ada.found().contains(what), "Ada's search finds {what}");
    }
    for what in ON_HER_DOOR {
        assert!(ada.published().contains(what), "Ada's door offers {what}");
    }

    // None of it reaches Bo: not his record, his window, his search or his door.
    nothing_of_reaches(&ada, &bo);

    // Through the door, Bo asks Ada's instance to archive her conversation: refused, and her
    // mail is as it was. Ada may.
    let archive = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.action_label == "Archive")
        .expect("Ada's conversations offer Archive")
        .address
        .clone();
    let heads = ada.hosted().record().doc().clone().get_heads();
    let invoke = |id: u64, actor: &str| CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: archive.resource.clone(),
        action: archive.action.clone(),
        argument: None,
        actor: actor.to_owned(),
        context: None,
    };
    ada.inbox.borrow_mut().push_back(invoke(1, BO));
    ada.settle();
    assert!(
        ada.answer(1).contains("permission_denied"),
        "Bo is refused: {}",
        ada.answer(1)
    );
    assert_eq!(
        ada.hosted().record().doc().clone().get_heads(),
        heads,
        "Ada's mail is as it was"
    );
    ada.inbox.borrow_mut().push_back(invoke(2, ADA));
    ada.settle();
    assert!(
        ada.answer(2).contains("Succeeded"),
        "Ada archives her own: {}",
        ada.answer(2)
    );
}
