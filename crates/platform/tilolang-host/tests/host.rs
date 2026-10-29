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

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
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

/// The broker's end of the instance's connection: what the instance sent, and what the test
/// delivers.
struct Broker {
    route: RouteSpec,
    sent: Sent,
    inbox: Inbox,
    joined: Option<RouteSpec>,
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
}

/// The window, as the test drives it from the far end of the patch connection.
struct Window {
    transport: ChannelTransport,
    root: Option<patch::Node>,
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
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::clone(&inbox),
        joined: None,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = RoomRecord::new(automerge::AutoCommit::new(), manager, route.clone(), person);
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

    // Ada has nothing of hers to undo: her Undo is refused, saying so.
    deliver(&inbox, 3, &undo, "ada");
    settle(&mut host, &mut window);
    assert!(
        matches!(
            answered(&sent, 3),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ),
        "ada's undo is refused: {:?}",
        answered(&sent, 3)
    );
    assert_eq!(value_of(host.instances().next().unwrap()), 0);

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
