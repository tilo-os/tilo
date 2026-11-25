//! Desktop `TiloLang` Mail uses one mailbox per workspace and applies workspace roles.

use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};
use tilo_action_policy::{
    Acting, Answer, Grant, Grantee, KindSet, MemberAccess, Role, WorkspacePolicy,
};
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

const MAIL: &str = "so.tilo.mail";

/// Two members, by the hex of their user root keys.
const ADA: &str = "abababababababababababababababababababababababababababababababab";
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// What only Ada's mail says: in her conversations' list and search, and on the API, which
/// names a conversation by its newest words.
const ADAS: [&str; 3] = ["Launch plan", "Grace Hopper", "lease renewal"];
/// Subjects her inbox's messages are published under: the launch thread's replies are its own
/// messages, and the lease's is its one, so the API offers Ada's messages and not only the
/// conversations they are in.
const ON_HER_MAIL: [&str; 2] = [
    "Re: Launch plan for the autumn release",
    "Your lease renewal",
];

/// The shipped Mail package, built with the packages it uses.
fn mail() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/mail");
    let platform = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
    let provided = tilolang::source::apps_of(&platform);
    let dirs = tilolang::source::dependency_order_with(&dir, &[], &provided)
        .expect("the packages resolve");
    let built =
        tilolang::source::compile_dirs(&dirs, &platform, &mut tilolang::OsEntropy::default())
            .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
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

/// Ada owns the workspace; Bo is one of its viewers.
fn policy(workspace: uuid::Uuid) -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(workspace.to_string(), ADA);
    policy.members.insert(BO.to_owned(), Role::Viewer);
    policy
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
    let mut policy = policy(workspace);
    policy.grants.push(Grant::new(
        "read-mail",
        Grantee::role(Role::Viewer),
        policy.node().app(MAIL),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(ADA),
    ));
    let hosted = Hosted::new(
        Spec::new(MAIL, workspace.to_string(), 1, person)
            .access(MemberAccess::new(policy, Acting::person(person))),
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::clone(bundle),
        &tilolang_process::hosted::Kept::default(),
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
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
fn workspace_members_share_mail_and_the_viewer_cannot_archive_it() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let bundle = Arc::new(mail());
    let workspace = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, workspace, ADA, &bundle);
    let mut bo = member(&bus, workspace, BO, &bundle);

    assert_eq!(ada.route, RouteSpec::app(MAIL, workspace));
    assert_eq!(bo.route, ada.route);
    assert!(collab_types::validator::requires_independent_document_keys(
        MAIL, ""
    ));
    assert!(collab_types::validator::requires_independent_document_keys(
        MAIL,
        &tilo_actions::document_sync::document_chunk("", 1),
    ));

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

    // The workspace has one mailbox: Bo, a viewer, reads the same mail, and his window offers
    // no Archive.
    for what in ADAS {
        assert!(bo.window.shows().contains(what), "Bo's Mail shows {what}");
        assert!(bo.found().contains(what), "Bo's search finds {what}");
    }
    assert!(
        bo.hosted()
            .published()
            .iter()
            .all(|action| action.action_label != "Archive"
                || !action.contract.availability.is_available()),
        "the viewer's command palette cannot archive a message"
    );

    // Through the API, Bo asks Ada's instance to archive a conversation: refused, and the mail
    // is as it was. Ada may.
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
    let published = ada.published();
    ada.inbox.borrow_mut().push_back(invoke(1, BO));
    ada.settle();
    assert!(
        ada.answer(1).contains("the action is not allowed"),
        "Bo is refused: {}",
        ada.answer(1)
    );
    assert_eq!(ada.published(), published, "the mail is as it was");
    ada.inbox.borrow_mut().push_back(invoke(2, ADA));
    ada.settle();
    assert!(
        ada.answer(2).contains("Succeeded"),
        "Ada archives the conversation: {}",
        ada.answer(2)
    );
    // Ada takes Bo out of the workspace, as a person does: his mail closes with it.
    let mut without_bo = policy(workspace);
    without_bo.members.remove(BO);
    let revoked = MemberAccess::new(without_bo, Acting::person(BO));
    assert!(bo.host.set_access(MAIL, Some(&revoked)));
    assert_eq!(bo.host.instances().count(), 0);
    assert_eq!(bo.host.take_ended(), [1]);
}

/// Ada's workspace, with a reply being written from her account and sent: the effect waits for
/// its author, and the store it is kept in is returned beside its entry's id.
fn sent_reply() -> (Rc<Bus>, String) {
    use collab_types::store::Query;
    use collab_types::store::wire::{Command, Reply};
    use tilolang_tilo::resources::OUTBOX;

    let bundle = Arc::new(mail());
    let workspace = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, workspace, ADA, &bundle);
    let mut bo = member(&bus, workspace, BO, &bundle);
    ada.settle();
    bo.settle();
    // A reply being written, from Ada's account: sending it queues the message to go.
    ada.host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a reply being written");
    ada.settle();
    bo.settle();
    let send = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.action_label == "Send")
        .expect("the draft offers Send")
        .address
        .clone();
    ada.inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id: 1,
        invocation: tilo_actions::InvocationId::new(),
        resource: send.resource.clone(),
        action: send.action,
        argument: None,
        actor: tilo_actions::MemberId::person(ADA).into(),
        context: None,
    });
    ada.settle();
    assert!(
        ada.answer(1).contains("Succeeded"),
        "Ada sends the reply: {}",
        ada.answer(1)
    );
    // It waits in her outbox, holding what it says and where it goes.
    let store = bus.workspace();
    let listed = store
        .command(Command::Query(Query::of(OUTBOX, 10)), ADA)
        .expect("the outbox is read");
    let Reply::Page(page) = listed else {
        panic!("the outbox reads as a page: {listed:?}");
    };
    let (id, entry) = page
        .resources
        .first()
        .expect("the send queues its message to go")
        .clone();
    assert!(
        entry.fields.contains_key("payload"),
        "Ada reads what her entry carries: {:?}",
        entry.fields
    );
    (bus, id)
}

/// A queued effect waits for its author alone: another member of the workspace reads none of it,
/// by its id or in the kind's list.
#[test]
fn another_member_reads_nothing_of_a_queued_effect() {
    use collab_types::store::Query;
    use collab_types::store::wire::{Command, Reply};
    use tilolang_tilo::resources::OUTBOX;

    let (bus, id) = sent_reply();
    let store = bus.workspace();
    let get = store
        .command(
            Command::Get {
                kind: OUTBOX.to_owned(),
                id,
            },
            BO,
        )
        .expect("the read is answered");
    assert!(
        matches!(get, Reply::Resource(None)),
        "Bo reads no entry of Ada's: {get:?}"
    );
    let listed = store
        .command(Command::Query(Query::of(OUTBOX, 10)), BO)
        .expect("the list is answered");
    let Reply::Page(page) = listed else {
        panic!("the outbox lists as a page: {listed:?}");
    };
    assert!(
        page.resources.is_empty(),
        "Bo lists none of Ada's outbox: {:?}",
        page.resources
    );
    let counted = store
        .command(
            Command::Count {
                kind: OUTBOX.to_owned(),
                filters: Vec::new(),
            },
            BO,
        )
        .expect("the count is answered");
    assert!(
        matches!(counted, Reply::Count(0)),
        "Bo counts none of Ada's outbox: {counted:?}"
    );
}

/// What search reads of a queued effect: another member finds nothing of it.
#[test]
fn another_members_search_finds_nothing_of_a_queued_effect() {
    use collab_types::store::Query;
    use collab_types::store::wire::{Command, Reply};
    use tilolang_tilo::resources::OUTBOX;

    let (bus, _) = sent_reply();
    let searched = bus
        .workspace()
        .command(
            Command::Fields {
                query: Query::of(OUTBOX, 10),
                fields: vec!["payload".to_owned(), "capability".to_owned()],
            },
            BO,
        )
        .expect("the search is answered");
    let Reply::Page(page) = searched else {
        panic!("search reads a page: {searched:?}");
    };
    assert!(
        page.resources.is_empty(),
        "Bo finds nothing of Ada's outbox: {:?}",
        page.resources
    );
}

/// Nobody outside the workspace reads its mail: a person who is no member of it asks another
/// member's instance for its account's mail and is allowed nothing of it. A member the workspace
/// removes loses its mail the same way, as the test above shows.
#[test]
fn nobody_outside_the_workspace_reads_its_mail() {
    const CY: &str = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef";

    let bundle = Arc::new(mail());
    let workspace = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, workspace, ADA, &bundle);
    let mut cy = member(&bus, workspace, CY, &bundle);

    // Ada's mail arrives.
    ada.settle();
    cy.settle();
    ada.host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a busy inbox");
    ada.settle();
    for what in ADAS {
        assert!(ada.window.shows().contains(what), "Ada's Mail shows {what}");
    }

    // Through the API, a person who is no member asks Ada's instance for an action of its mail:
    // refused, and nothing is read or changed for them.
    let archive = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.action_label == "Archive")
        .expect("Ada's conversations offer Archive")
        .address
        .clone();
    ada.inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id: 1,
        invocation: tilo_actions::InvocationId::new(),
        resource: archive.resource.clone(),
        action: archive.action,
        argument: None,
        actor: tilo_actions::MemberId::person(CY).into(),
        context: None,
    });
    ada.settle();
    assert!(
        ada.answer(1).contains("the action is not allowed"),
        "an outsider is refused: {}",
        ada.answer(1)
    );
}
