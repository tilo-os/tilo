//! A tilolang host runs an app's instance without the toolkit: it drives a window in another
//! end of a patch connection, commits what the person did there to the room record on the
//! instance's own connection, takes it back on the person's Undo, publishes the app's actions on
//! that connection and runs what the broker delivers there as its invoker's step. An instance ends
//! when its window closes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test: a failure is a panic"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::action_directory::{
    ActionDirectoryPage, ActionDirectoryQuery, ActionDirectoryResolution,
};
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PeerTransport,
    RouteSpec,
};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Lane, ToApp, ToUi, Transport as _};
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::room::RoomRecord;
use tilolang_tilo::runtime::Value;

const APP: &str = "so.tilo.lang.counter";

fn counter() -> Bundle {
    let lock =
        tilolang::IdsLock::parse(include_str!("../../../../apps/tl/counter/app.ids")).unwrap();
    let compiled = tilolang::compile_package(
        include_str!("../../../../apps/tl/counter/app.tl"),
        &lock,
        &[],
    )
    .unwrap_or_else(|_| panic!("the counter compiles"));
    Bundle::new(&compiled, [])
}

type Sent = Rc<RefCell<Vec<CollabRequest>>>;
type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// The instance's action directory as the broker holds it: the apps it was offered under, and
/// what the test asks of it.
#[derive(Default)]
struct Directory {
    offered: Vec<String>,
    asked: Vec<ActionDirectoryRequest>,
}

/// The broker's end of the instance's connection: what the instance sent, and what the test
/// delivers.
struct Broker {
    route: RouteSpec,
    sent: Sent,
    inbox: Inbox,
    joined: Option<RouteSpec>,
    directory: Rc<RefCell<Directory>>,
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if matches!(request, CollabRequest::Hello { .. }) {
            self.joined = Some(self.route.clone());
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

    fn publish_action_directory(&mut self, app_id: String) -> Result<(), String> {
        self.directory.borrow_mut().offered.push(app_id);
        Ok(())
    }

    fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        std::mem::take(&mut self.directory.borrow_mut().asked)
    }
}

/// The window, as the test drives it from the far end of the patch connection.
struct Window {
    transport: ChannelTransport,
    root: Option<patch::Node>,
    /// The tree of the lane beside the root: the item the window shows.
    detail: Option<patch::Node>,
    /// The resource the detail lane shows.
    detail_target: Option<patch::ResourceRef>,
    /// The instance asked the window to come forward.
    activated: bool,
}

impl Window {
    /// Take in what the instance sent: the root's tree, kept up to date with its patches.
    fn take(&mut self) {
        while let Some(batch) = self.transport.recv().unwrap() {
            for message in patch::decode_batch::<ToUi>(&batch).unwrap() {
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
                    ToUi::Open {
                        lane: Lane::Detail,
                        target,
                        tree,
                        ..
                    } => {
                        self.detail = Some(tree);
                        self.detail_target = Some(target);
                    }
                    ToUi::Patch {
                        lane: Lane::Detail,
                        patches,
                    } => {
                        let detail = self.detail.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(detail).expect("the patch applies");
                        }
                    }
                    ToUi::Activate => self.activated = true,
                    _ => {}
                }
            }
        }
    }

    fn send(&mut self, messages: &[ToApp]) {
        self.transport
            .send(patch::encode_batch(messages).unwrap())
            .unwrap();
    }

    /// The key of the counter's last button, its increment, and the event it fires.
    fn increment(&self) -> (String, String) {
        fn buttons<'a>(node: &'a patch::Node, out: &mut Vec<&'a patch::Node>) {
            if node.kind == "ui.Button" {
                out.push(node);
            }
            for child in &node.children {
                buttons(child, out);
            }
        }
        let mut found = Vec::new();
        buttons(self.root.as_ref().expect("the root is shown"), &mut found);
        let plus = found.last().expect("the counter shows its buttons");
        let event = plus.events.keys().next().expect("the button fires").clone();
        (plus.key.clone(), event)
    }
}

fn value_of(hosted: &Hosted) -> i64 {
    let instance = hosted.instance();
    let host = instance.host().unwrap();
    let root = host.root_of(APP).unwrap();
    match host.runtime().read(&root, "value").unwrap() {
        Value::Int(value) => value,
        other => panic!("the counter's value is {other:?}"),
    }
}

/// A host with one counter instance on a broker the test plays, and its window.
static WOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn wake() -> tilolang_tilo::background::Waker {
    Arc::new(|| WOKEN.store(true, std::sync::atomic::Ordering::SeqCst))
}

fn running(person: &str) -> (TilolangHost, Window, Sent, Inbox, RouteSpec) {
    running_app(person, APP, counter())
}

/// [`running`] with an instance of `bundle`, the package `app`.
fn running_app(
    person: &str,
    app: &str,
    bundle: Bundle,
) -> (TilolangHost, Window, Sent, Inbox, RouteSpec) {
    running_with(person, app, bundle, Rc::default())
}

/// [`running_app`], the broker holding the instance's action directory in `directory`.
fn running_with(
    person: &str,
    app: &str,
    bundle: Bundle,
    directory: Rc<RefCell<Directory>>,
) -> (TilolangHost, Window, Sent, Inbox, RouteSpec) {
    running_on(person, app, bundle, directory, automerge::AutoCommit::new())
}

/// [`running_with`], the record holding `doc` as the room handed it.
fn running_on(
    person: &str,
    app: &str,
    bundle: Bundle,
    directory: Rc<RefCell<Directory>>,
    doc: automerge::AutoCommit,
) -> (TilolangHost, Window, Sent, Inbox, RouteSpec) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(app, uuid::Uuid::new_v4());
    let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::clone(&inbox),
        joined: None,
        directory,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = RoomRecord::new(doc, manager, route.clone(), person);
    let mut host = TilolangHost::new(None, wake());
    let kept = host.kept(app).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        Spec {
            app: app.to_owned(),
            room: route.cluster_id.to_string(),
            instance: 1,
            person: person.to_owned(),
        },
        record,
        Arc::new(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        Rc::new(Phrases::new("en-US")),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
    };
    (host, window, sent, inbox, route)
}

/// Run the host as its loop does: a frame whenever something woke it, until nothing has for a
/// while (work off the frame, the record's publication, wakes it when it is ready).
fn settle(host: &mut TilolangHost, window: &mut Window) {
    let mut quiet_since = std::time::Instant::now();
    loop {
        let woken = WOKEN.swap(false, std::sync::atomic::Ordering::SeqCst);
        let changed = host.turn();
        window.take();
        if changed || woken {
            quiet_since = std::time::Instant::now();
        } else if quiet_since.elapsed() > std::time::Duration::from_millis(300) {
            return;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

fn documents_sent(sent: &Sent) -> usize {
    sent.borrow()
        .iter()
        .filter(|request| matches!(request, CollabRequest::Document { .. }))
        .count()
}

/// A host whose counter instance opens its record as the host does, joined on a broker the test
/// plays, from the copy `documents` keeps: no committed read, as a sealed room's record the room
/// does not hold yet opens.
fn opened_from_own_copy(
    person: &str,
    documents: &Rc<dyn durable_store::DurableStore>,
) -> (TilolangHost, Window, Sent, RouteSpec) {
    opened_from_own_copy_in(person, documents, RouteSpec::app(APP, uuid::Uuid::new_v4()))
}

/// [`opened_from_own_copy`], on `route`.
fn opened_from_own_copy_in(
    person: &str,
    documents: &Rc<dyn durable_store::DurableStore>,
    route: RouteSpec,
) -> (TilolangHost, Window, Sent, RouteSpec) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let sent: Sent = Rc::default();
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::default(),
        joined: None,
        directory: Rc::default(),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = RoomRecord::open_read(manager, None, documents, person).unwrap();
    let mut host = TilolangHost::new(None, wake());
    let kept = host.kept(APP).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        Spec {
            app: APP.to_owned(),
            room: route.cluster_id.to_string(),
            instance: 1,
            person: person.to_owned(),
        },
        record,
        Arc::new(counter()),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        Rc::new(Phrases::new("en-US")),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
    };
    (host, window, sent, route)
}

/// What the room holds of the record: every change the instance sent on its connection, and
/// nothing else, as a sealed room's channel keeps it.
fn room_copy(sent: &Sent) -> automerge::AutoCommit {
    let mut room = automerge::AutoCommit::new();
    for request in sent.borrow().iter() {
        if let CollabRequest::Document { mutation, .. } = request {
            let bytes = match mutation {
                collab_types::DocumentMutation::Incremental(bytes)
                | collab_types::DocumentMutation::Snapshot(bytes) => bytes,
            };
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, bytes).unwrap();
        }
    }
    room
}

#[test]
fn a_record_made_from_the_hosts_own_copy_reopens_from_what_its_room_holds() {
    // A record the room did not hold opens from the host's own copy, whose making (its schema
    // stamp) is a change of this replica's. A press is the instance's first change to the room.
    let documents: Rc<dyn durable_store::DurableStore> =
        Rc::new(durable_store::MemoryStore::default());
    let (mut host, mut window, sent, _route) = opened_from_own_copy("ada", &documents);
    settle(&mut host, &mut window);
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    settle(&mut host, &mut window);
    assert_eq!(value_of(host.instances().next().unwrap()), 1);

    // The room's copy alone is the whole record: nothing it holds builds on a change that never
    // reached it, so a reader that has only the room's writes can read it back.
    let room = room_copy(&sent);
    assert_eq!(
        automerge::ReadDoc::get_missing_deps(&room, &[]),
        Vec::new(),
        "every change the room's writes build on reached the room"
    );

    // A fresh host, opened on the room's copy as a reopen reads it, shows the press.
    let (mut reopened, mut window, _sent, _inbox, _route) =
        running_on("ada", APP, counter(), Rc::default(), room);
    settle(&mut reopened, &mut window);
    assert_eq!(
        value_of(reopened.instances().next().unwrap()),
        1,
        "the reopened instance shows the record as it was"
    );
}

#[test]
fn a_press_in_the_window_is_a_step_on_the_instances_connection_and_undo_takes_it_back() {
    let (mut host, mut window, sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    assert!(window.root.is_some(), "the window shows the counter");
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 0);
    assert!(
        hosted
            .published()
            .iter()
            .any(|action| action.address.resource.starts_with("tl:")
                && action.action_label == "Increment"),
        "the instance publishes the counter's actions: {:?}",
        hosted
            .published()
            .iter()
            .map(|a| (&a.address, &a.action_label))
            .collect::<Vec<_>>()
    );
    assert!(
        sent.borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::PublishActionSurface { app_id, .. } if app_id == APP)),
        "published on its own connection, under its app"
    );

    // The person presses + in the window: the instance runs it and commits it as their step.
    let before = documents_sent(&sent);
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 1, "the press reached the runtime");
    assert!(
        documents_sent(&sent) > before,
        "the change went out on the connection"
    );
    assert_eq!(
        hosted.record().can_undo(),
        (true, false),
        "it is ada's step"
    );

    // Undo from the window takes it back, as a change of its own sent on the connection.
    let before = documents_sent(&sent);
    window.send(&[ToApp::Undo]);
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 0, "undo took the press back");
    assert!(
        documents_sent(&sent) > before,
        "the undo went out on the connection"
    );
    assert!(
        hosted.record().can_undo().1,
        "the window's Undo is ada's history undo: she can redo it"
    );

    // Redo from the window brings it back, the same way.
    let before = documents_sent(&sent);
    window.send(&[ToApp::Redo]);
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 1, "redo brought the press back");
    assert!(
        documents_sent(&sent) > before,
        "the redo went out on the connection"
    );
    assert_eq!(
        hosted.record().can_undo(),
        (true, false),
        "and it is ada's to undo again"
    );
}

#[test]
fn what_the_broker_delivers_runs_as_its_invokers_step() {
    let (mut host, mut window, sent, inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    let increment = hosted
        .published()
        .iter()
        .find(|action| {
            action.address.resource.starts_with("tl:") && action.action_label == "Increment"
        })
        .unwrap_or_else(|| {
            panic!(
                "increment is published: {:?}",
                hosted
                    .published()
                    .iter()
                    .map(|a| (&a.address, &a.action_label))
                    .collect::<Vec<_>>()
            )
        })
        .address
        .clone();
    let ada_could = hosted.record().can_undo();
    let invocation = tilo_actions::InvocationId::new();
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id: 7,
        invocation,
        resource: increment.resource.clone(),
        action: increment.action,
        argument: None,
        actor: "grace".to_owned(),
        context: None,
    });
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 1, "the delivered action ran");
    assert!(
        sent.borrow().iter().any(|request| matches!(
            request,
            CollabRequest::UpdateInvocation {
                id: 7,
                update: tilo_actions::InvocationUpdate::Succeeded { .. }
            }
        )),
        "the broker is told it succeeded"
    );
    assert_eq!(
        hosted.record().can_undo_as("grace"),
        (true, false),
        "the step is grace's, who invoked it"
    );
    assert_eq!(
        hosted.record().can_undo(),
        ada_could,
        "it gives ada nothing more to undo"
    );
}

/// The answer the broker was told for invocation `id`.
fn answered(sent: &Sent, id: u64) -> Option<tilo_actions::InvocationUpdate> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::UpdateInvocation {
                id: answered,
                update,
            } if *answered == id => Some(update.clone()),
            _ => None,
        })
}

fn deliver(inbox: &Inbox, id: u64, address: &tilo_actions::ActionAddress, actor: &str) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: address.resource.clone(),
        action: address.action.clone(),
        argument: None,
        actor: actor.to_owned(),
        context: None,
    });
}

#[test]
fn the_instance_publishes_and_answers_the_history_verbs_every_app_does() {
    let (mut host, mut window, sent, inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    let find = |label: &str| {
        hosted
            .published()
            .iter()
            .find(|action| {
                action.address.resource == format!("tl:{APP}") && action.action_label == label
            })
            .unwrap_or_else(|| {
                panic!(
                    "{label} is published on the app's resource: {:?}",
                    hosted
                        .published()
                        .iter()
                        .map(|a| (&a.address, &a.action_label))
                        .collect::<Vec<_>>()
                )
            })
            .address
            .clone()
    };
    let (undo, redo, history) = (find("Undo"), find("Redo"), find("History"));
    let increment = hosted
        .published()
        .iter()
        .find(|action| action.action_label == "Increment")
        .unwrap()
        .address
        .clone();

    // Grace increments through the broker, then undoes through the broker: her own step.
    deliver(&inbox, 1, &increment, "grace");
    settle(&mut host, &mut window);
    assert_eq!(value_of(host.instances().next().unwrap()), 1);
    deliver(&inbox, 2, &undo, "grace");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    assert_eq!(value_of(hosted), 0, "grace's undo took her step back");
    match answered(&sent, 2) {
        Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
            let value = output.value.expect("undo says what it took back");
            assert_eq!(value["undone"].as_array().map(Vec::len), Some(1), "{value}");
        }
        other => panic!("undo was answered {other:?}"),
    }
    assert_eq!(hosted.record().can_undo_as("grace"), (false, true));

    // Ada's one step is the counter she created, which Grace has since counted in: her Undo
    // takes that step back without taking Grace's work with it, so the counter stays.
    deliver(&inbox, 3, &undo, "ada");
    settle(&mut host, &mut window);
    match answered(&sent, 3) {
        Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
            let value = output.value.expect("undo says what it took back");
            assert_eq!(value["undone"][0]["sentence"], "Created Counter", "{value}");
        }
        other => panic!("ada's undo is never refused: {other:?}"),
    }
    assert_eq!(value_of(host.instances().next().unwrap()), 0);
    assert!(
        !host
            .instances()
            .next()
            .unwrap()
            .record()
            .can_undo_as("ada")
            .0
    );

    // Grace's redo brings it back, and History lists all three steps as hers.
    deliver(&inbox, 4, &redo, "grace");
    settle(&mut host, &mut window);
    assert_eq!(
        value_of(host.instances().next().unwrap()),
        1,
        "redo brought it back"
    );
    deliver(&inbox, 5, &history, "grace");
    settle(&mut host, &mut window);
    match answered(&sent, 5) {
        Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
            let value = output.value.expect("history lists the versions");
            let hers = value["versions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|version| version["actor"]["id"] == "grace")
                .count();
            assert_eq!(hers, 3, "{value}");
        }
        other => panic!("history was answered {other:?}"),
    }
}

#[test]
fn the_instance_answers_read_and_show_this_window_as_every_app_does() {
    let (mut host, mut window, sent, inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    let find = |id: &str| {
        hosted
            .published()
            .iter()
            .find(|action| {
                action.address.resource == format!("tl:{APP}") && action.address.action == id
            })
            .unwrap_or_else(|| panic!("{id} is published on the app's resource"))
            .address
            .clone()
    };
    let (read, activate) = (
        find("read"),
        find(tilo_actions::remote::APPLICATION_ACTIVATE_ACTION),
    );

    deliver(&inbox, 1, &read, "grace");
    settle(&mut host, &mut window);
    match answered(&sent, 1) {
        Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
            let value = output.value.expect("read answers what the record holds");
            assert!(value.get("records").is_some(), "{value}");
        }
        other => panic!("read was answered {other:?}"),
    }

    deliver(&inbox, 2, &activate, "grace");
    settle(&mut host, &mut window);
    assert!(
        matches!(
            answered(&sent, 2),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "Show this window is answered"
    );
    assert!(window.activated, "the window was asked to come forward");
}

#[test]
fn an_instance_ends_when_its_window_closes() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    assert!(!host.is_empty());
    drop(window);
    host.turn();
    assert!(host.is_empty(), "the instance ended with its window");
}

/// A broker that answers the join only once the test lets it: an instance whose workspace is slow
/// to answer.
struct SlowBroker {
    route: RouteSpec,
    answer: Rc<std::cell::Cell<bool>>,
    asked: bool,
}

impl PeerTransport for SlowBroker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if matches!(request, CollabRequest::Hello { .. }) {
            self.asked = true;
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        (self.asked && self.answer.get()).then(|| {
            self.asked = false;
            Ok(self.route.clone())
        })
    }
}

/// The value the counter's window shows, as the root's tree holds it.
fn shown(window: &Window) -> String {
    fn texts(node: &patch::Node, out: &mut Vec<String>) {
        for value in node.props.values() {
            out.push(format!("{value:?}"));
        }
        for child in &node.children {
            texts(child, out);
        }
    }
    let mut out = Vec::new();
    texts(window.root.as_ref().expect("the root is shown"), &mut out);
    out.join(" ")
}

#[test]
fn a_slow_open_does_not_hold_up_another_instances_frame() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let before = shown(&window);

    // A second instance whose workspace has not answered its join yet.
    let answer = Rc::new(std::cell::Cell::new(false));
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let mut manager = CollaborationManager::with_peer_transport(SlowBroker {
        route: route.clone(),
        answer: Rc::clone(&answer),
        asked: false,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some("grace".to_owned()),
    }));
    let (ours, theirs) = Channel::pair().unwrap();
    host.open(
        Spec {
            app: APP.to_owned(),
            room: route.cluster_id.to_string(),
            instance: 2,
            person: "grace".to_owned(),
        },
        Arc::new(counter()),
        manager,
        ours,
    );
    let mut second = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
    };

    // The first instance's window presses +: its next frame comes at once, while the second
    // instance still waits for its workspace.
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    let started = std::time::Instant::now();
    while shown(&window) == before {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the first instance's frame waited for the second's open"
        );
        host.turn();
        window.take();
    }
    assert_eq!(host.opening(), 1, "the second instance is still opening");
    assert_eq!(host.instances().count(), 1);
    assert!(second.root.is_none());

    // Its workspace answers: it opens and shows its window.
    answer.set(true);
    let started = std::time::Instant::now();
    while second.root.is_none() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the second instance never opened"
        );
        host.turn();
        window.take();
        second.take();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(host.opening(), 0);
    assert_eq!(host.instances().count(), 2);
}

/// How many of `rounds` waits of the host's loop would end at once, nothing written to its wake:
/// each a wait that asks whether anything has something for the host, without waiting.
fn wakeups(host: &TilolangHost, rounds: usize) -> usize {
    let wake = tilolang_host::wait::Wake::new().unwrap();
    (0..rounds)
        .filter(|_| {
            tilolang_host::wait::wait(None, wake.fd(), host, 0)
                .unwrap()
                .any()
        })
        .count()
}

#[test]
fn the_hosts_wake_ends_one_wait_per_writing_once_consumed() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let wake = tilolang_host::wait::Wake::new().unwrap();
    let waker = wake.waker();
    let woken = |wake: &tilolang_host::wait::Wake| {
        (0..100)
            .filter(|_| {
                tilolang_host::wait::wait(None, wake.fd(), &host, 0)
                    .unwrap()
                    .wake
            })
            .count()
    };
    assert_eq!(woken(&wake), 0, "an unwritten wake ended a wait");
    for _ in 0..1000 {
        waker();
    }
    assert!(woken(&wake) > 0, "a written wake did not end the wait");
    wake.consume().unwrap();
    assert_eq!(woken(&wake), 0, "the consumed wake still ended waits");
}

#[test]
fn an_idle_hosts_loop_does_not_wake_until_a_window_has_something() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    assert_eq!(
        wakeups(&host, 100),
        0,
        "an idle running instance woke its host"
    );

    // A second instance whose workspace has not answered its join, its window sending before
    // the instance runs: what it sent waits for the instance, and the loop sleeps meanwhile.
    let answer = Rc::new(std::cell::Cell::new(false));
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let mut manager = CollaborationManager::with_peer_transport(SlowBroker {
        route: route.clone(),
        answer: Rc::clone(&answer),
        asked: false,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some("grace".to_owned()),
    }));
    let (ours, theirs) = Channel::pair().unwrap();
    host.open(
        Spec {
            app: APP.to_owned(),
            room: route.cluster_id.to_string(),
            instance: 2,
            person: "grace".to_owned(),
        },
        Arc::new(counter()),
        manager,
        ours,
    );
    let mut second = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
    };
    second.send(&[ToApp::Fire {
        lane: Lane::Root,
        key: "early".to_owned(),
        event: "pressed before it opened".to_owned(),
        value: None,
    }]);
    host.turn();
    assert_eq!(host.opening(), 1);
    assert_eq!(
        wakeups(&host, 100),
        0,
        "what an opening instance's window sent woke its host before the instance could read it"
    );

    // What a running window sends wakes the loop, and its frame takes it.
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    assert_eq!(
        wakeups(&host, 1),
        1,
        "a press in a running window did not wake its host"
    );
    host.turn();
    window.take();
    assert_eq!(wakeups(&host, 100), 0, "the frame left the press unread");

    // An opening instance's window closing wakes the loop, which ends that opening.
    drop(second);
    assert_eq!(
        wakeups(&host, 1),
        1,
        "an opening window's closing did not wake its host"
    );
    host.turn();
    assert_eq!(host.opening(), 0);
    assert_eq!(host.take_ended(), vec![2]);
    assert_eq!(wakeups(&host, 100), 0);
}

/// Ask the instance's directory `query`, as the broker does, and take its answer.
fn page(
    host: &mut TilolangHost,
    window: &mut Window,
    directory: &Rc<RefCell<Directory>>,
    query: ActionDirectoryQuery,
) -> ActionDirectoryPage {
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    directory
        .borrow_mut()
        .asked
        .push(ActionDirectoryRequest::Query { query, reply });
    host.turn();
    settle(host, window);
    answer.try_recv().expect("the instance answered the query")
}

fn query() -> ActionDirectoryQuery {
    ActionDirectoryQuery {
        peer_id: 1,
        revision: String::new(),
        cursor: Vec::new(),
        needle: String::new(),
        parent_resource: String::new(),
        max_results: 64,
        max_bytes: 64 * 1024,
        max_work: 512,
        max_depth: 4,
        resource_name: String::new(),
    }
}

#[test]
fn the_instance_offers_an_action_directory_the_door_and_agents_browse() {
    let directory = Rc::new(RefCell::new(Directory::default()));
    let (mut host, mut window, _sent, _inbox, _route) =
        running_with("ada", APP, counter(), Rc::clone(&directory));
    settle(&mut host, &mut window);
    assert_eq!(
        directory.borrow().offered,
        [APP],
        "the instance offers its directory on its connection, under its app, once"
    );
    let published = host.instances().next().unwrap().published().to_vec();

    // The top level names the app's resources: what makes the instance browsable.
    let top = page(&mut host, &mut window, &directory, query());
    assert!(top.exhausted && !top.revision.is_empty(), "{top:?}");
    let app = format!("tl:{APP}");
    let root = top
        .rows
        .iter()
        .find(|row| row.resource == app)
        .unwrap_or_else(|| panic!("the app's resource is at the top: {:?}", top.rows))
        .clone();
    assert!(root.action.is_none() && root.has_children);

    // Its level lists its actions, each as published.
    let level = page(
        &mut host,
        &mut window,
        &directory,
        ActionDirectoryQuery {
            parent_resource: app.clone(),
            ..query()
        },
    );
    let read = level
        .rows
        .iter()
        .find(|row| row.resource == app && row.action.as_deref() == Some("read"))
        .unwrap_or_else(|| panic!("read is on the app's level: {:?}", level.rows));
    assert!(read.descriptor.is_some());

    // A name finds the app's resource by its kind.
    let named = page(
        &mut host,
        &mut window,
        &directory,
        ActionDirectoryQuery {
            resource_name: root.resource_kind,
            ..query()
        },
    );
    assert!(
        named.rows.iter().any(|row| row.resource == app),
        "{named:?}"
    );

    // A search finds the counter's Increment, and it resolves at the page's revision.
    let found = page(
        &mut host,
        &mut window,
        &directory,
        ActionDirectoryQuery {
            needle: "increment".into(),
            ..query()
        },
    );
    let increment = found
        .rows
        .iter()
        .find(|row| {
            row.descriptor
                .as_ref()
                .is_some_and(|action| action.action_label == "Increment")
        })
        .unwrap_or_else(|| panic!("the search finds Increment: {:?}", found.rows));
    assert!(published.iter().any(|action| {
        action.address.resource == increment.resource
            && Some(action.address.action.as_str()) == increment.action.as_deref()
    }));
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    directory
        .borrow_mut()
        .asked
        .push(ActionDirectoryRequest::Resolve {
            revision: found.revision.clone(),
            resource: increment.resource.clone(),
            action: increment.action.clone().unwrap(),
            reply,
        });
    host.turn();
    match answer.try_recv().expect("the instance resolved it") {
        ActionDirectoryResolution::Found { row, revision } => {
            assert_eq!(row.action, increment.action);
            assert_eq!(revision.as_deref(), Some(found.revision.as_str()));
        }
        other => panic!("Increment resolves: {other:?}"),
    }
}

const NOTES: &str = "so.tilo.lang.notes";

/// The shipped Notes package, built with the packages it uses.
fn notes() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/notes");
    let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
}

/// The node of `tree` whose prop `name` reads `value`.
fn node_with<'a>(tree: &'a patch::Node, name: &str, value: &str) -> Option<&'a patch::Node> {
    if tree
        .props
        .get(name)
        .is_some_and(|prop| format!("{prop:?}").contains(value))
    {
        return Some(tree);
    }
    tree.children
        .iter()
        .find_map(|child| node_with(child, name, value))
}

/// How often the phase `name` ran.
fn calls(phases: &[tilo_resource::profiling::Sample], name: &str) -> usize {
    phases
        .iter()
        .find(|phase| phase.phase == name)
        .map_or(0, |phase| phase.calls)
}

/// What the instance did while the person typed into a note, and what it publishes then.
struct Typed {
    /// How often it built its whole surface to publish it.
    built: usize,
    /// How often it published only the labels that moved.
    relabeled: usize,
    /// The resource labels it publishes, as it holds them.
    labels: Vec<String>,
    /// The resource labels the broker holds, from every delta the instance sent it.
    broker: Vec<String>,
    /// The resource labels the instance's directory finds for what was typed, asked after.
    found: Vec<String>,
}

/// The resource labels of the surface the broker holds once it applied every delta the
/// instance sent.
fn broker_labels(sent: &Sent) -> Vec<String> {
    let mut table = collaboration::SurfaceTable::default();
    for request in sent.borrow().iter() {
        if let CollabRequest::PublishActionSurface { delta, .. } = request {
            table.apply(delta).expect("the broker takes every delta");
        }
    }
    table
        .actions()
        .into_iter()
        .map(|action| action.resource_label)
        .collect()
}

/// A notebook of `held` notes, the first of them open in the window as the person made it; what
/// the instance did while the person typed `typed` into the open note's field whose placeholder
/// is `field`, a key at a time, each followed by the host's frames until it is quiet, as its loop
/// runs them.
fn typing(held: usize, field: &str, typed: &str) -> Typed {
    let directory = Rc::new(RefCell::new(Directory::default()));
    let (mut host, mut window, sent, inbox, _route) =
        running_with("ada", NOTES, notes(), Rc::clone(&directory));
    settle(&mut host, &mut window);
    // The person starts a note from the empty notebook: it opens beside the list.
    let button = node_with(window.root.as_ref().unwrap(), "label", "New note")
        .expect("the empty notebook offers a new note")
        .clone();
    let event = button
        .events
        .keys()
        .next()
        .expect("the button fires")
        .clone();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key: button.key,
        event,
        value: None,
    }]);
    settle(&mut host, &mut window);
    // An agent adds the rest, which moves nobody's view.
    let new_note = host
        .instances()
        .next()
        .unwrap()
        .published()
        .iter()
        .find(|action| action.action_label == "New note")
        .expect("New note is published")
        .address
        .clone();
    for id in 1..held {
        deliver(&inbox, (id as u64).saturating_add(100), &new_note, "ada");
    }
    settle(&mut host, &mut window);
    let input = node_with(
        window.detail.as_ref().expect("the note is open"),
        "placeholder",
        field,
    )
    .unwrap_or_else(|| panic!("the open note has a field `{field}`"))
    .key
    .clone();
    let target = window.detail_target.clone().expect("the note is open");
    let documents = documents_sent(&sent);
    let (mut built, mut relabeled) = (0, 0);
    let mut shown = String::new();
    for (seq, key) in typed.chars().enumerate() {
        let ((), phases) = tilo_resource::profiling::observe(|| {
            window.send(&[ToApp::Splice {
                lane: Lane::Detail,
                target: target.clone(),
                key: input.clone(),
                seq: (seq as u64).saturating_add(1),
                at: u32::try_from(shown.len()).unwrap(),
                delete: 0,
                insert: key.to_string(),
            }]);
            settle(&mut host, &mut window);
        });
        shown.push(key);
        built = calls(&phases, "tl.host.publish").saturating_add(built);
        relabeled = calls(&phases, "tl.host.relabel").saturating_add(relabeled);
    }
    assert!(
        documents_sent(&sent) > documents,
        "the typing went out on the connection"
    );
    let labels = host
        .instances()
        .next()
        .unwrap()
        .published()
        .iter()
        .map(|action| action.resource_label.clone())
        .collect();
    let found = page(
        &mut host,
        &mut window,
        &directory,
        ActionDirectoryQuery {
            needle: typed.to_owned(),
            ..query()
        },
    )
    .rows
    .into_iter()
    .map(|row| row.resource_label)
    .collect();
    Typed {
        built,
        relabeled,
        labels,
        broker: broker_labels(&sent),
        found,
    }
}

#[test]
fn typing_a_note_builds_the_surface_only_for_what_it_shows_whatever_the_notebook() {
    // A new note's title is empty, so its body's first text is its label until it has a
    // title: each key of the body publishes that label alone, with one note or with two
    // hundred, never the whole notebook's surface.
    for held in [1, 200] {
        let body = typing(held, "Write something", "draft");
        assert_eq!(
            (body.built, body.relabeled),
            (0, 5),
            "typing a body published the surface (whole, labels) with {held} notes"
        );
    }
    // A note's title is its label: each key publishes that label alone, the same work with one
    // note as with two hundred, and never the whole surface.
    for held in [1, 200] {
        let title = typing(held, "Untitled", "abc");
        assert_eq!(
            title.built, 0,
            "typing a title built the whole surface with {held} notes"
        );
        assert_eq!(
            title.relabeled, 3,
            "typing a title published its label {} times for 3 keys with {held} notes",
            title.relabeled
        );
        assert!(
            title.labels.iter().any(|label| label == "abc"),
            "the instance names the note by its new title with {held} notes: {:?}",
            title.labels
        );
        assert!(
            title.broker.iter().any(|label| label == "abc"),
            "the broker's copy names the note by its new title with {held} notes"
        );
        assert!(
            title.found.iter().any(|label| label == "abc"),
            "the instance's directory finds the note by its new title with {held} notes: {:?}",
            title.found
        );
        assert!(
            !title
                .broker
                .iter()
                .any(|label| label == "a" || label == "ab"),
            "the broker's copy still names the note by an earlier title with {held} notes"
        );
    }
}

/// The title of the note `target` names, as the instance's runtime reads it.
fn title_of(host: &TilolangHost, target: &patch::ResourceRef) -> String {
    let instance = host.instances().next().unwrap().instance();
    let resource = tilolang_tilo::instance::wire::runtime_resource(target).unwrap();
    match instance
        .host()
        .unwrap()
        .runtime()
        .read(&resource, "title")
        .unwrap()
    {
        Value::Text(title) => title,
        other => panic!("a note's title is {other:?}"),
    }
}

#[test]
fn an_edit_lands_in_the_note_it_was_typed_into_whatever_the_window_shows_when_it_arrives() {
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", NOTES, notes());
    settle(&mut host, &mut window);
    let new_note = |window: &mut Window| {
        let button = node_with(window.root.as_ref().unwrap(), "label", "New note")
            .expect("the notebook offers a new note")
            .clone();
        let event = button.events.keys().next().unwrap().clone();
        window.send(&[ToApp::Fire {
            lane: Lane::Root,
            key: button.key,
            event,
            value: None,
        }]);
    };
    new_note(&mut window);
    settle(&mut host, &mut window);
    let first = window
        .detail_target
        .clone()
        .expect("the first note is open");
    let title = node_with(window.detail.as_ref().unwrap(), "placeholder", "Untitled")
        .expect("the note has its title field")
        .key
        .clone();
    // The person typed into the first note's title; the window sent it just as their next
    // press made a second note, which the instance shows by the time the typing arrives.
    new_note(&mut window);
    window.send(&[ToApp::Splice {
        lane: Lane::Detail,
        target: first.clone(),
        key: title,
        seq: 1,
        at: 0,
        delete: 0,
        insert: "Plan ".to_owned(),
    }]);
    settle(&mut host, &mut window);
    let second = window
        .detail_target
        .clone()
        .expect("the second note is open");
    assert_ne!(first, second, "the press opened the second note");
    assert_eq!(
        title_of(&host, &first),
        "Plan ",
        "the typing landed in the note it was typed into"
    );
    assert_eq!(
        title_of(&host, &second),
        "",
        "and none of it in the note shown when it arrived"
    );
}

#[test]
fn the_instance_answers_a_late_joiners_snapshot_request_with_its_record() {
    let (mut host, mut window, sent, inbox, route) = running("ada");
    settle(&mut host, &mut window);
    let snapshots = |sent: &Sent| {
        sent.borrow()
            .iter()
            .filter(|request| {
                matches!(
                    request,
                    CollabRequest::Document {
                        mutation: collaboration::DocumentMutation::Snapshot(_),
                        ..
                    }
                )
            })
            .count()
    };
    let before = snapshots(&sent);
    inbox
        .borrow_mut()
        .push_back(CollabEvent::DocumentSnapshotRequested { route });
    settle(&mut host, &mut window);
    assert_eq!(
        snapshots(&sent),
        before + 1,
        "the record went to the late joiner"
    );
}

#[test]
fn the_instance_answers_every_handoff_step_the_shell_asks() {
    use collaboration::handoff::{HandoffReport, HandoffRequest, Successor};
    let (mut host, mut window, sent, inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let reports = |sent: &Sent| -> Vec<HandoffReport> {
        sent.borrow()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::ReportHandoff { message, .. } => HandoffReport::decode(message),
                _ => None,
            })
            .collect()
    };
    inbox.borrow_mut().push_back(CollabEvent::Handoff {
        request: HandoffRequest::Checkpoint { token: 7 },
    });
    inbox.borrow_mut().push_back(CollabEvent::Handoff {
        request: HandoffRequest::Retire {
            token: 8,
            successor: Successor {
                room: uuid::Uuid::new_v4().to_string(),
                record: "elsewhere".to_owned(),
                by: "ada".to_owned(),
            },
        },
    });
    settle(&mut host, &mut window);
    let reports = reports(&sent);
    assert!(
        reports
            .iter()
            .any(|report| matches!(report, HandoffReport::Done { token: 7, .. })),
        "a checkpoint is done: {reports:?}"
    );
    assert!(
        reports.iter().any(|report| matches!(
            report,
            HandoffReport::Failed { token: 8, reason } if !reason.is_empty()
        )),
        "a retirement is refused with why: {reports:?}"
    );
}

/// The origin of a counter's record for `person`, on a broker the test plays, taking its
/// change on the far end of the returned connection.
fn applier(
    person: &str,
    doc: automerge::AutoCommit,
) -> (
    tilolang_host::hosted::Applier,
    std::os::unix::net::UnixStream,
    Sent,
) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let sent: Sent = Rc::default();
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::default(),
        joined: None,
        directory: Rc::default(),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = RoomRecord::new(doc, manager, route.clone(), person);
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let applier = tilolang_host::hosted::Applier::open(
        Spec {
            app: APP.to_owned(),
            room: route.cluster_id.to_string(),
            instance: 1 << 62,
            person: person.to_owned(),
        },
        record,
        &counter(),
        &Phrases::new("en-US"),
        wake(),
        theirs.into(),
    )
    .unwrap();
    (applier, ours, sent)
}

/// Where the counter's Increment is published on a record of the room, and that record: the
/// one a task read, whose resources its change names.
fn increment() -> (tilo_actions::ActionAddress, automerge::AutoCommit) {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    let address = hosted
        .published()
        .iter()
        .find(|action| {
            action.address.resource.starts_with("tl:") && action.action_label == "Increment"
        })
        .unwrap()
        .address
        .clone();
    (address, hosted.record().doc().clone())
}

/// Hand `applier` `change` and run it until it answers: the answer.
fn apply(
    applier: &mut tilolang_host::hosted::Applier,
    connection: &std::os::unix::net::UnixStream,
    change: &runtime_portal::tilolang_host::Change,
) -> serde_json::Value {
    use std::io::{BufRead as _, Write as _};
    let mut line = serde_json::to_vec(change).unwrap();
    line.push(b'\n');
    (&*connection).write_all(&line).unwrap();
    for _ in 0..200 {
        applier.turn();
        if applier.answered() {
            let mut answer = String::new();
            std::io::BufReader::new(connection)
                .read_line(&mut answer)
                .unwrap();
            return serde_json::from_str(&answer).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the change was never answered");
}

/// An agent task acting for ada, as the broker admitted it.
fn task_invoker() -> serde_json::Value {
    serde_json::to_value(
        tilo_actions::ActionInvoker::actor("agent:scribe")
            .with_kind(tilo_actions::ActorKind::Agent)
            .acting_for("ada"),
    )
    .unwrap()
}

#[test]
fn a_tasks_change_runs_at_the_records_origin_as_its_step_with_no_instance() {
    let (address, doc) = increment();
    let (mut applier, connection, sent) = applier("ada", doc);
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: address.resource,
            action: address.action,
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: None,
        },
    );
    assert!(answer.get("ok").is_some(), "{answer}");
    assert_eq!(answer["differs"], false, "{answer}");
    assert!(
        answer["change"]
            .as_str()
            .is_some_and(|change| !change.is_empty()),
        "it says what it wrote, for the task to read back: {answer}"
    );
    assert!(
        documents_sent(&sent) > 0,
        "what it wrote was handed to the room"
    );
    // A preview made from an earlier read that no longer holds is applied as the record is
    // now, and the answer says it differed.
    let (address, doc) = increment();
    let (mut applier, connection, _sent) = self::applier("ada", doc);
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: address.resource,
            action: address.action,
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: Some(serde_json::json!({"planned": "from an earlier read"})),
        },
    );
    assert!(answer.get("ok").is_some(), "{answer}");
    assert_eq!(answer["differs"], true, "{answer}");
}

#[test]
fn a_change_the_record_cannot_take_is_refused_with_why() {
    let (mut applier, connection, sent) = applier("ada", automerge::AutoCommit::new());
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: "tl:so.tilo.lang.counter".to_owned(),
            action: "no-such-action".to_owned(),
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: None,
        },
    );
    let why = answer["failed"]
        .as_str()
        .unwrap_or_else(|| panic!("{answer}"));
    assert!(!why.is_empty());
    assert_eq!(documents_sent(&sent), 0, "nothing was written");
}

#[test]
fn a_change_of_the_root_no_window_made_yet_makes_it_first() {
    let (address, _window_record) = increment();
    assert!(address.resource.contains(":root-"), "{}", address.resource);
    // No window ever opened the app here: its record holds no root.
    let (mut applier, connection, sent) = applier("ada", automerge::AutoCommit::new());
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: address.resource,
            action: address.action,
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: None,
        },
    );
    assert!(answer.get("ok").is_some(), "{answer}");
    let wrote = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(answer["change"].as_str().unwrap())
            .unwrap()
    };
    let mut read = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut read, &wrote).unwrap();
    assert!(
        read.get_missing_deps(&[]).is_empty(),
        "what it says it wrote holds the root it made, for a task that read no record"
    );
    assert!(
        documents_sent(&sent) > 0,
        "the root and the change went to the room"
    );
}

/// A change its room refuses is taken back out of the record at its origin, and the task is
/// answered why: the record is never left holding, building on and keeping a change the room
/// has none of. Here the surface the room decides the record's changes by publishes the
/// counter's record but none of its actions.
#[test]
fn a_change_the_room_refuses_is_never_kept() {
    let (address, doc) = increment();
    collaboration::app_surface::register_package(
        APP,
        b"[[resources]]\nkind = \"tl.so.tilo.lang.counter.Counter\"\nlabel = \"Counter\"\nversion = 1\n",
    );
    let before = doc.clone().get_heads();
    let (mut applier, connection, sent) = applier("ada", doc);
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: address.resource,
            action: address.action,
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: None,
        },
    );
    let why = answer["failed"]
        .as_str()
        .unwrap_or_else(|| panic!("refused, with why: {answer}"));
    assert!(why.contains("publishes no"), "{why}");
    assert_eq!(
        applier.record().doc().clone().get_heads(),
        before,
        "the record holds nothing of the refused change"
    );
    assert_eq!(documents_sent(&sent), 0, "nothing was handed to the room");
}

/// A record an older store wrote, whose first change is its schema stamp committed with no
/// message, is handed to its room whole on the next send, every change of it claimed: the
/// stamp, a root change with no deps that only sets the schema version, as the record's
/// genesis. No change is rewritten and no id changes.
#[test]
fn a_record_whose_first_change_is_an_unnamed_stamp_is_handed_to_its_room_whole() {
    let documents: Rc<dyn durable_store::DurableStore> =
        Rc::new(durable_store::MemoryStore::default());
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    // The record as the store wrote it before a record's first change named itself.
    let stamp = {
        let mut store = collaboration::record::save::room_document(
            &documents,
            APP,
            &route.cluster_id.to_string(),
        )
        .unwrap();
        let mut doc = store.new_document();
        let stamp = doc.commit().unwrap();
        store.save(&mut doc).unwrap();
        stamp
    };
    let (mut host, mut window, sent, _route) = opened_from_own_copy_in("ada", &documents, route);
    settle(&mut host, &mut window);
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    settle(&mut host, &mut window);
    assert_eq!(value_of(host.instances().next().unwrap()), 1);
    let room = room_copy(&sent);
    assert!(
        automerge::ReadDoc::get_change_by_hash(&room, &stamp).is_some(),
        "the stamp, unchanged, reached the room"
    );
    assert_eq!(
        automerge::ReadDoc::get_missing_deps(&room, &[]),
        Vec::new(),
        "the room holds the whole record"
    );
    for request in sent.borrow().iter() {
        if let CollabRequest::Document { mutation, .. } = request {
            let claims =
                collaboration::app_surface::claims_of(APP, mutation).unwrap_or_else(|error| {
                    panic!("every change the room is handed is claimed: {error}")
                });
            if let Some(claim) = claims.iter().find(|claim| claim.change == stamp.0) {
                assert_eq!(
                    claim.action,
                    collab_types::ClaimedAction::Framework(collab_types::FrameworkChange::Genesis)
                );
            }
        }
    }
}
