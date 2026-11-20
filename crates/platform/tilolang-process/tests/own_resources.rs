//! Mail is each member's own. A member runs Mail in a tilolang host on that person's own document
//! of the workspace, on a route of its own, and the instance refuses what another member asks of it
//! through the API.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test: a failure is a panic"
)]

use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Level, ToUi, Transport as _};
use tilolang_process::TilolangProcess;
use tilolang_process::connection::Connection;
use tilolang_process::hosted::{Hosted, Spec};
use tilolang_process::phrases::Phrases;
use tilolang_process::testing::{Broker, Bus, Inbox, Sent};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::resources::ResourceStore;

const MAIL: &str = "so.tilo.lang.mail";

/// Two members, by the hex of their user root keys.
const ADA: &str = "abababababababababababababababababababababababababababababababab";
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// What only Ada's mail says: in her conversations' list and search, and on the API, which
/// names a conversation by its newest words.
const ADAS: [&str; 3] = ["Launch plan", "Grace Hopper", "lease renewal"];
/// The senders Ada's conversations are offered under on the API: what her window's rows are
/// labeled by, and so what the instance publishes of them.
const ON_HER_MAIL: [&str; 2] = ["launch@rivera.example", "lease@lettings.example"];

fn compiled(dir: &str, source: &str, ids: &str) -> Bundle {
    let lock = tilolang::IdsLock::parse(ids).unwrap();
    let compiled =
        tilolang::compile_package(source, &lock, &[]).unwrap_or_else(|d| panic!("{dir}: {d:#?}"));
    Bundle::new(&compiled, [])
}

fn mail() -> Bundle {
    compiled(
        "mail",
        include_str!("../../../../apps/tl/mail/app.tl"),
        include_str!("../../../../apps/tl/mail/app.ids"),
    )
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
                        level: Level::Root,
                        tree,
                        ..
                    } => self.root = Some(tree),
                    ToUi::Patch {
                        level: Level::Root,
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
    host: TilolangProcess,
    window: Window,
    inbox: Inbox,
    sent: Sent,
    route: RouteSpec,
}

fn wake() -> tilolang_tilo::background::Waker {
    Arc::new(|| {})
}

fn member(bus: &Rc<Bus>, workspace: uuid::Uuid, person: &str, bundle: &Arc<Bundle>) -> Member {
    let route = tilolang_process::route_of(&bundle.meta, workspace, person);
    let broker = Broker::on(bus, route.clone(), person);
    let (inbox, sent) = (broker.inbox(), broker.sent());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let mut host = TilolangProcess::new(wake());
    let hosted = Hosted::new(
        Spec::new(MAIL, workspace.to_string(), 1, person),
        connection,
        store,
        tilolang_process::hosted::memory_content(),
        Arc::clone(bundle),
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
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

    /// Frames until the host owes nothing: no instance being opened, and no window with a frame
    /// coming — a change its instance has not sent, or work it reads off the frame. Nothing is
    /// settled by letting time pass; a host that owes nothing draws nothing more.
    fn settle(&mut self) {
        let began = std::time::Instant::now();
        loop {
            let changed = self.host.turn();
            self.window.take();
            if !changed && !self.host.owes_work() {
                return;
            }
            assert!(
                began.elapsed() < std::time::Duration::from_secs(120),
                "the host owes work after {}s: {} opening, {} running",
                began.elapsed().as_secs(),
                self.host.opening(),
                self.host.instances().count()
            );
            if !changed {
                // Nothing moved: what the host owes is work off its frame, which wakes it.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// What search finds in the member's Mail, as text.
    fn found(&self) -> String {
        let host = self.hosted().instance().host().unwrap();
        format!("{:?}", tilolang_tilo::search::every_hit(&host))
    }

    /// What the member's instance publishes on the API (each resource with its actions), as
    /// text.
    fn published(&self) -> String {
        format!("{:?}", self.hosted().published())
    }

    /// The answer the member's instance sent to the API's invocation `id`.
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

#[test]
fn mail_is_each_members_own_document_of_the_workspace() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let bundle = Arc::new(mail());
    assert!(bundle.meta.own_resources, "Mail declares `resources: .own`");
    let workspace = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, workspace, ADA, &bundle);
    let mut bo = member(&bus, workspace, BO, &bundle);

    // Each member's Mail is on their own document of the workspace, which only their devices are
    // admitted to; an app that shares its documents stays on the workspace's document of the app.
    assert_eq!(
        ada.route.document.as_deref(),
        tilolang_tilo::member_document_of("so.tilo.lang.mail", ADA).as_deref()
    );
    assert_eq!(
        bo.route.document.as_deref(),
        tilolang_tilo::member_document_of("so.tilo.lang.mail", BO).as_deref()
    );
    assert_ne!(ada.route.document, bo.route.document, "no document is shared");
    let counter = compiled(
        "counter",
        include_str!("../../../../apps/tl/counter/app.tl"),
        include_str!("../../../../apps/tl/counter/app.ids"),
    );
    assert_eq!(
        tilolang_process::route_of(&counter.meta, workspace, ADA),
        RouteSpec::app("so.tilo.lang.counter", workspace),
        "a shared app's document is the workspace's"
    );
    assert_eq!(
        tilolang_process::route_of(&bundle.meta, workspace, "ada"),
        RouteSpec::app(MAIL, workspace),
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
    for what in ON_HER_MAIL {
        assert!(
            ada.published().contains(what),
            "Ada's api offers {what}: {}",
            ada.published()
        );
    }

    // Through the API, Bo asks Ada's instance to archive her conversation: refused, and her
    // mail is as it was. Ada may.
    let archive = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.action_label == "Archive")
        .expect("Ada's conversations offer Archive")
        .address
        .clone();
    let invoke = |id: u64, actor: &str| CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: archive.resource.clone(),
        action: archive.action.clone(),
        argument: None,
        actor: tilo_actions::MemberId::person(actor).into(),
        context: None,
    };
    ada.inbox.borrow_mut().push_back(invoke(1, BO));
    ada.settle();
    assert!(
        ada.answer(1).contains("permission_denied"),
        "Bo is refused: {}",
        ada.answer(1)
    );
    assert!(
        ada.window.shows().contains("Launch plan"),
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
