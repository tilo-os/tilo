//! A tilolang host runs an app's instance without the toolkit: it drives a window in another
//! end of a patch connection, commits what the person did there to the workspace record on the
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
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use collaboration::action_directory::{
    ActionDirectoryPage, ActionDirectoryQuery, ActionDirectoryResolution,
};
use collaboration::commit_log::CommitHistory;
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PeerTransport,
    RouteSpec, WorkspaceCommit,
};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Lane, ToApp, ToUi, Transport as _};
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::record::InstanceRecord;
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
/// The actions a broker learned, as its log keeps them.
type Actions = Rc<RefCell<Vec<CommitHistory>>>;

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
    /// The actions the broker learned, as a peer's log keeps them: what one undo takes back
    /// across the workspace.
    actions: Actions,
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if matches!(request, CollabRequest::Hello { .. }) {
            self.joined = Some(self.route.clone());
        }
        if let CollabRequest::Document {
            commit: Some(commit),
            ..
        } = &request
        {
            self.actions.borrow_mut().push(CommitHistory::of(commit));
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }

    fn action_of(&self, workspace: &str, change: &[u8; 32]) -> Option<CommitHistory> {
        self.actions
            .borrow()
            .iter()
            .rev()
            .find(|action| {
                action.workspace == workspace
                    && action
                        .parts
                        .iter()
                        .any(|part| part.changes.contains(change))
            })
            .cloned()
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
    /// What the instance says each bound control holds, by key: its type is what a window
    /// reads to know how a field takes typing.
    bound: BTreeMap<String, patch::Value>,
    /// The form the instance shows for the action waiting for its arguments, as the window
    /// last had it.
    form: Option<Box<patch::Form>>,
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
                    ToUi::Bound { key, value, .. } => {
                        self.bound.insert(key, value);
                    }
                    ToUi::Form(form) => self.form = form,
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

/// [`running_with`], the record holding `doc` as the workspace handed it.
fn running_on(
    person: &str,
    app: &str,
    bundle: Bundle,
    directory: Rc<RefCell<Directory>>,
    doc: automerge::AutoCommit,
) -> (TilolangHost, Window, Sent, Inbox, RouteSpec) {
    let route = RouteSpec::app(app, uuid::Uuid::new_v4());
    let mut host = TilolangHost::new(None, wake());
    let (window, sent, inbox) = instance_on(
        &mut host,
        spec(1, person, &route),
        bundle,
        &route,
        directory,
        Rc::default(),
        doc,
    );
    (host, window, sent, inbox, route)
}

/// What the instance `number` for `person` on `route` is.
fn spec(number: u64, person: &str, route: &RouteSpec) -> Spec {
    Spec {
        app: route.app_id.clone(),
        workspace: route.cluster_id.to_string(),
        instance: number,
        person: person.to_owned(),
    }
}

/// The instance `attributes` names, of `bundle`, in `host` on `route`, over `doc` as the workspace
/// handed it: its window, what its connection sent and what the broker holds for it. The broker
/// keeps the actions it learns in `actions`.
fn instance_on(
    host: &mut TilolangHost,
    attributes: Spec,
    bundle: Bundle,
    route: &RouteSpec,
    directory: Rc<RefCell<Directory>>,
    actions: Actions,
    doc: automerge::AutoCommit,
) -> (Window, Sent, Inbox) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::clone(&inbox),
        joined: None,
        directory,
        actions,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(attributes.person.clone()),
    }));
    let _ = manager.poll();
    let record = InstanceRecord::new(doc, manager, route.clone(), &attributes.person);
    let kept = host.kept(&attributes.app).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        attributes,
        record,
        Arc::new(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Phrases::new("en-US"),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
        bound: BTreeMap::new(),
        form: None,
    };
    (window, sent, inbox)
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
/// plays, from the copy `documents` keeps: no committed read, as a sealed workspace's record the workspace
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
        actions: Rc::default(),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = InstanceRecord::open_read(manager, None, documents, person).unwrap();
    let mut host = TilolangHost::new(None, wake());
    let kept = host.kept(APP).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        Spec {
            app: APP.to_owned(),
            workspace: route.cluster_id.to_string(),
            instance: 1,
            person: person.to_owned(),
        },
        record,
        Arc::new(counter()),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Phrases::new("en-US"),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window {
        transport: ChannelTransport::new(theirs).unwrap(),
        root: None,
        detail: None,
        detail_target: None,
        activated: false,
        bound: BTreeMap::new(),
        form: None,
    };
    (host, window, sent, route)
}

/// What the workspace holds of the record: every change the instance sent on its connection, and
/// nothing else, as a sealed workspace's channel keeps it.
fn workspace_copy(sent: &Sent) -> automerge::AutoCommit {
    let mut workspace = automerge::AutoCommit::new();
    for request in sent.borrow().iter() {
        if let CollabRequest::Document { mutation, .. } = request {
            let bytes = match mutation {
                collab_types::DocumentMutation::Incremental(bytes)
                | collab_types::DocumentMutation::Snapshot(bytes) => bytes,
            };
            tilo_automerge::ApplyIncoming::load_incoming(&mut workspace, bytes).unwrap();
        }
    }
    workspace
}

#[test]
fn a_record_made_from_the_hosts_own_copy_reopens_from_what_its_workspace_holds() {
    // A record the workspace did not hold opens from the host's own copy, whose making (its schema
    // stamp) is a change of this replica's. A press is the instance's first change to the workspace.
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

    // The workspace's copy alone is the whole record: nothing it holds builds on a change that never
    // reached it, so a reader that has only the workspace's writes can read it back.
    let workspace = workspace_copy(&sent);
    assert_eq!(
        automerge::ReadDoc::get_missing_deps(&workspace, &[]),
        Vec::new(),
        "every change the workspace's writes build on reached the workspace"
    );

    // A fresh host, opened on the workspace's copy as a reopen reads it, shows the press.
    let (mut reopened, mut window, _sent, _inbox, _route) =
        running_on("ada", APP, counter(), Rc::default(), workspace);
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
            .any(|request| matches!(request, CollabRequest::PublishActionSurface { .. })),
        "published on its own connection"
    );

    // The person presses + in the window: its control is bound to the instance's own action, so
    // the window sends the invocation, and the instance runs it and commits it as their step.
    let before = documents_sent(&sent);
    let address = hosted
        .published()
        .iter()
        .find(|action| action.action_label == "Increment")
        .expect("the instance publishes the action the button runs")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
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

/// The last change a window's press sent the workspace.
/// The last commit the connection sent a change under.
fn sent_under(sent: &Sent) -> Option<WorkspaceCommit> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::Document {
                commit: Some(commit),
                ..
            } => Some(commit.clone()),
            _ => None,
        })
}

/// A counter record holding one change of the action `invocation` names, as an action that wrote
/// the unit left it: the endpoints restarted since, so nothing but the record remembers it.
fn seeded(person: &str, invocation: tilo_actions::InvocationId) -> automerge::AutoCommit {
    let mut doc = automerge::AutoCommit::new();
    let principal = tilolang_tilo::runtime::Principal::new(person);
    let host = tilolang_tilo::host::Host::new(&doc, principal.clone(), || None).unwrap();
    let package = host
        .load_builtin(
            include_str!("../../../../apps/tl/counter/app.tl"),
            include_str!("../../../../apps/tl/counter/app.ids"),
        )
        .unwrap();
    let root = host.root_of(APP).unwrap();
    for committed in host.take_committed() {
        applied(&mut doc, &committed.action, person, None);
    }
    let increment = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "increment")
        .unwrap()
        .id
        .clone();
    host.runtime()
        .invoke_action(
            &principal,
            (increment.as_str(), vec![Value::resource(&root)]),
        )
        .unwrap();
    for committed in host.take_committed() {
        applied(&mut doc, &committed.action, person, Some(invocation));
    }
    doc
}

/// Apply `action` to `doc` as the record does: for `person`, in the invocation `invocation` when
/// the change is part of an action's.
fn applied(
    doc: &mut automerge::AutoCommit,
    action: &tilolang_tilo::action::DynAction,
    person: &str,
    invocation: Option<tilo_actions::InvocationId>,
) {
    let context = tilo_actions::ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("workspace"),
        &action.kind,
        &action.kind,
    )
    .with_source_invocation(invocation)
    .with_invoker(tilo_actions::ActionInvoker::actor(person));
    tilo_actions::apply_local_with_context(
        doc,
        action,
        action.encode().unwrap(),
        Some(person.as_bytes()),
        context,
    )
    .unwrap();
    doc.commit();
}

fn values(host: &TilolangHost) -> Vec<i64> {
    host.instances().map(value_of).collect()
}

#[test]
fn one_undo_takes_back_every_unit_of_an_action_and_sends_the_reverts_as_one_action() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let cluster = uuid::Uuid::new_v4();
    let mut host = TilolangHost::new(None, wake());
    let actions: Actions = Rc::default();
    // The person's one action wrote both units, as a run does: each record holds the change
    // the action's invocation names. The endpoint restarted since, so no log remembers it.
    let invocation = tilo_actions::InvocationId::new();
    let first_route = RouteSpec::record(APP, cluster, "a");
    let (mut first, first_sent, _) = instance_on(
        &mut host,
        spec(1, "ada", &first_route),
        counter(),
        &first_route,
        Rc::default(),
        Rc::clone(&actions),
        seeded("ada", invocation),
    );
    let second_route = RouteSpec::record(APP, cluster, "b");
    let (mut second, second_sent, _) = instance_on(
        &mut host,
        spec(2, "ada", &second_route),
        counter(),
        &second_route,
        Rc::default(),
        Rc::default(),
        seeded("ada", invocation),
    );
    settle(&mut host, &mut first);
    second.take();
    assert_eq!(
        values(&host),
        vec![1, 1],
        "both units hold the action's change"
    );

    // One undo, asked at one window: every unit the action wrote takes its step back, and the
    // reverts go out as one action of their own.
    first.send(&[ToApp::Undo]);
    settle(&mut host, &mut first);
    second.take();
    assert_eq!(
        values(&host),
        vec![0, 0],
        "one undo took the action back in both units"
    );
    let (undo_a, undo_b) = (
        sent_under(&first_sent).expect("the first unit's revert is part of an action"),
        sent_under(&second_sent).expect("the second unit's revert is part of an action"),
    );
    assert_eq!(
        undo_a.id(),
        undo_b.id(),
        "both units reverted as one action"
    );
    assert_eq!(
        undo_a.parts.len(),
        2,
        "the undo names both units it reverted"
    );
    assert_eq!(undo_a.parts, undo_b.parts);
    assert_ne!(
        undo_a.invocation,
        Some(invocation.0),
        "the undo ran under an invocation of its own"
    );
    // The two units reverted through their own live instances: each record holds the undo as its
    // own step, which the person can redo.
    let redoable: Vec<bool> = host
        .instances()
        .map(|hosted| hosted.record().can_undo().1)
        .collect();
    assert_eq!(redoable, vec![true, true], "each unit can redo its revert");

    // The workspace was handed the undo as one action naming both units it reverted.
    let history = actions.borrow();
    assert!(!history.is_empty(), "the undo reached the workspace");
    for action in history.iter() {
        assert_eq!(action.workspace, cluster.to_string());
        assert_eq!(action.commit, undo_a.id());
        assert_eq!(action.parts.len(), 2, "the undo names both units");
    }
    drop(history);
    let undo = host
        .instances()
        .next()
        .unwrap()
        .record()
        .manager()
        .action_of(&cluster.to_string(), &undo_a.parts[0].changes[0])
        .expect("the log knows the undo");
    assert_eq!(undo.commit, undo_a.id());
    assert_eq!(undo.parts.len(), 2);
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
            workspace: route.cluster_id.to_string(),
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
        bound: BTreeMap::new(),
        form: None,
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
            workspace: route.cluster_id.to_string(),
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
        bound: BTreeMap::new(),
        form: None,
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

/// Where the shipped tilolang packages are.
fn shipped_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

/// The shipped package in `dir`, built with the packages it uses, and its app id.
fn shipped(dir: &std::path::Path) -> (String, Bundle) {
    // The platform's apps (the device, Settings) and the native apps beside the packages (the
    // session's `so.tilo.session`) are described from their surfaces, not built.
    let published =
        tilolang_tilo::catalogue::published_packages(&tilolang::source::search_of(dir, &[]))
            .expect("the published surfaces read");
    let built = tilolang::source::compile_package(
        dir,
        &[],
        &published,
        &mut tilolang::OsEntropy::default(),
    )
    .unwrap_or_else(|error| panic!("{}: the packages compile: {error}", dir.display()));
    let (package, used) = built.split_last().expect("the package is built last");
    (
        package.compiled.meta.app.clone(),
        Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled)),
    )
}

/// The shipped Notes package, built with the packages it uses.
fn notes() -> Bundle {
    shipped(&shipped_dir().join("notes")).1
}

/// Every package shipped in `apps/tl` publishes what it offers as a valid resource/action surface:
/// hosted as a window opens it, the host refuses none of it and the broker holds it. A resource's
/// views each open through an action of their own, so none shares one.
#[test]
fn every_shipped_package_publishes_a_surface_the_host_does_not_refuse() {
    let mut dirs: Vec<_> = std::fs::read_dir(shipped_dir())
        .expect("the shipped packages")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|dir| dir.join("app.tl").is_file())
        .collect();
    dirs.sort();
    assert!(dirs.len() >= 2, "{dirs:?}");
    for dir in dirs {
        let (app, bundle) = shipped(&dir);
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
        settle(&mut host, &mut window);
        let hosted = host.instances().next().expect("the instance runs");
        assert!(
            hosted.refused().is_empty(),
            "{app}: the host refused its surface: {:#?}",
            hosted.refused()
        );
        assert!(
            !hosted.published().is_empty(),
            "{app}: the broker holds nothing it offers"
        );
    }
}

/// A view other than its type's default is opened by the action published with it: the
/// counter's `controls`, invoked as the broker delivers it, shows the counter through that
/// view, its two buttons without the count.
#[test]
fn a_published_view_opens_through_its_own_action() {
    let (mut host, mut window, sent, inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let hosted = host.instances().next().unwrap();
    let opens = |label: &str| {
        hosted
            .published()
            .iter()
            .find(|action| action.view.as_ref().is_some_and(|view| view.label == label))
            .unwrap_or_else(|| panic!("the view `{label}` is published with its open action"))
            .clone()
    };
    let (counter, controls) = (opens("counter"), opens("controls"));
    assert!(counter.view.as_ref().unwrap().primary);
    assert_eq!(counter.address.action, "open");
    assert_ne!(
        controls.address.action, counter.address.action,
        "each view opens through an action of its own"
    );
    assert!(window.detail.is_none(), "the window shows its root alone");
    deliver(&inbox, 41, &controls.address, "ada");
    settle(&mut host, &mut window);
    assert!(
        matches!(
            answered(&sent, 41),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "{:?}",
        answered(&sent, 41)
    );
    let shown = format!("{:?}", window.detail.as_ref().expect("the view opened"));
    assert!(
        shown.contains("plus") && shown.contains("minus") && !shown.contains("Count"),
        "the counter, through its controls: {shown}"
    );
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

/// What a rich field holds once `text` was typed into it: one paragraph of it.
fn typed_rich(text: &str) -> patch::rich::RichText {
    patch::rich::RichText {
        blocks: vec![patch::rich::Block {
            kind: "paragraph".to_owned(),
            text: text.to_owned(),
            ..patch::rich::Block::default()
        }],
        embeds: Vec::new(),
    }
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
    /// How many records publication looked at while the keys were typed.
    visited: u64,
    /// How many times it read every record in full while the keys were typed.
    swept: u64,
}

/// How many records the instance's publication has looked at so far.
fn visited(host: &TilolangHost) -> u64 {
    host.instances()
        .next()
        .expect("the app runs")
        .instance()
        .visited_records()
}

/// How many times the instance's publication has read every record in full so far.
fn swept(host: &TilolangHost) -> u64 {
    host.instances()
        .next()
        .expect("the app runs")
        .instance()
        .sweeps()
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

/// The event that puts `shown` into the field `input`: a rich field is edited whole, as the
/// toolkit sends what the person typed into it, a text field by the splice each key is.
fn key_edit(
    rich: bool,
    target: &patch::ResourceRef,
    input: &str,
    seq: usize,
    at: u32,
    shown: &str,
    key: char,
) -> ToApp {
    if rich {
        ToApp::EditRich {
            lane: Lane::Detail,
            target: target.clone(),
            key: input.to_owned(),
            rich: typed_rich(shown),
        }
    } else {
        ToApp::Splice {
            lane: Lane::Detail,
            target: target.clone(),
            key: input.to_owned(),
            seq: (seq as u64).saturating_add(1),
            at,
            delete: 0,
            insert: key.to_string(),
        }
    }
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
    let field_node = node_with(
        window.detail.as_ref().expect("the note is open"),
        "placeholder",
        field,
    )
    .unwrap_or_else(|| panic!("the open note has a field `{field}`"));
    let input = field_node.key.clone();
    // The field's own value says how it takes typing, as a window reads it: a rich text is
    // edited whole, a text by the splice each key is.
    let rich = match window.bound.get(&input) {
        Some(patch::Value::Rich(_)) => true,
        Some(_) => false,
        None => panic!("the instance says what the field `{field}` holds"),
    };
    let target = window.detail_target.clone().expect("the note is open");
    let documents = documents_sent(&sent);
    let (mut built, mut relabeled) = (0, 0);
    let mut seen: u64 = 0;
    let mut sweeps: u64 = 0;
    let mut shown = String::new();
    for (seq, key) in typed.chars().enumerate() {
        let at = u32::try_from(shown.len()).unwrap();
        shown.push(key);
        let edit = key_edit(rich, &target, &input, seq, at, &shown, key);
        let (before, before_sweeps) = (visited(&host), swept(&host));
        let ((), phases) = tilo_resource::profiling::observe(|| {
            window.send(&[edit]);
            settle(&mut host, &mut window);
        });
        seen = seen.saturating_add(visited(&host).saturating_sub(before));
        sweeps = sweeps.saturating_add(swept(&host).saturating_sub(before_sweeps));
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
        visited: seen,
        swept: sweeps,
    }
}

#[test]
fn typing_a_note_builds_the_surface_only_for_what_it_shows_whatever_the_notebook() {
    // A new note's title is empty, so its body's first text is its label until it has a
    // title: each key of the body publishes that label alone, with one note or with two
    // hundred, never the whole notebook's surface.
    let mut held_by = Vec::new();
    for held in [1, 200] {
        let body = typing(held, "Write something", "draft");
        assert_eq!(
            (body.built, body.relabeled),
            (0, 5),
            "typing a body published the surface (whole, labels) with {held} notes"
        );
        held_by.push((held, body.visited));
        assert_eq!(
            body.swept, 0,
            "typing a body read every record in full {} times with {held} notes",
            body.swept
        );
    }
    // A key republishes the note it typed into, and no other record the notebook holds: what a
    // turn costs is the same with one note as with two hundred.
    assert!(
        held_by[0].1 >= 5 && held_by[0].1 <= 10,
        "five keys looked at {} records",
        held_by[0].1
    );
    assert_eq!(
        held_by[0].1, held_by[1].1,
        "five keys looked at {} records with one note, {} with 200",
        held_by[0].1, held_by[1].1
    );
    // A note's title is its label: each key publishes that label alone, the same work with one
    // note as with two hundred, and never the whole surface.
    let mut held_by = Vec::new();
    for held in [1, 200] {
        let title = typing(held, "Untitled", "abc");
        held_by.push((held, title.visited));
        assert_eq!(
            title.swept, 0,
            "typing a title read every record in full {} times with {held} notes",
            title.swept
        );
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
    assert!(
        held_by[0].1 >= 3 && held_by[0].1 <= 6,
        "three keys looked at {} records",
        held_by[0].1
    );
    assert_eq!(
        held_by[0].1, held_by[1].1,
        "three keys looked at {} records with one note, {} with 200",
        held_by[0].1, held_by[1].1
    );
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
    applier_of(person, APP, &counter(), doc)
}

/// The origin of `app`'s records, built as `bundle`, for `person`, holding `doc`: as
/// [`applier`] is the counter's.
fn applier_of(
    person: &str,
    app: &str,
    bundle: &Bundle,
    doc: automerge::AutoCommit,
) -> (
    tilolang_host::hosted::Applier,
    std::os::unix::net::UnixStream,
    Sent,
) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(app, uuid::Uuid::new_v4());
    let sent: Sent = Rc::default();
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::default(),
        joined: None,
        directory: Rc::default(),
        actions: Rc::default(),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record = InstanceRecord::new(doc, manager, route.clone(), person);
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let applier = tilolang_host::hosted::Applier::open(
        Spec {
            app: app.to_owned(),
            workspace: route.cluster_id.to_string(),
            instance: 1 << 62,
            person: person.to_owned(),
        },
        record,
        bundle,
        &Phrases::new("en-US"),
        wake(),
        theirs.into(),
    )
    .unwrap();
    (applier, ours, sent)
}

/// Where the counter's Increment is published on a record of the workspace, and that record: the
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
        "what it wrote was handed to the workspace"
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
        "the root and the change went to the workspace"
    );
}

/// A package may declare several roots: a change of any of them that no window made yet makes
/// that root first. The shell's top bar is its `shell` root, not the root it opens with.
#[test]
fn a_change_of_any_root_no_window_made_yet_makes_that_root_first() {
    let (app, bundle) = shipped(&shipped_dir().join("shell"));
    let shell = bundle
        .meta
        .named_roots
        .iter()
        .find(|root| root.name == "shell")
        .expect("the shell's top bar is a root of its own")
        .ty
        .clone();
    assert_ne!(
        bundle.meta.roots.first(),
        Some(&shell),
        "not the root the shell opens with"
    );
    let hide = &bundle
        .meta
        .actions
        .iter()
        .find(|action| action.name == "hide_app_menus")
        .expect("the shell's top bar has its actions")
        .id;
    let (mut applier, connection, sent) =
        applier_of("ada", &app, &bundle, automerge::AutoCommit::new());
    let answer = apply(
        &mut applier,
        &connection,
        &runtime_portal::tilolang_host::Change {
            resource: tilolang_tilo::publish::resource_id(&app, &format!("root-{shell}")),
            action: hide.clone(),
            arguments: serde_json::Map::new(),
            invoker: task_invoker(),
            context: None,
            preview: None,
        },
    );
    assert!(answer.get("ok").is_some(), "{answer}");
    assert!(
        documents_sent(&sent) > 0,
        "the root and the change went to the workspace"
    );
}

/// A change its workspace refuses is taken back out of the record at its origin, and the task is
/// answered why: the record is never left holding, building on and keeping a change the workspace
/// has none of. Here the surface the workspace decides the record's changes by publishes the
/// counter's record but none of its actions.
#[test]
fn a_change_the_workspace_refuses_is_never_kept() {
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
    assert_eq!(
        documents_sent(&sent),
        0,
        "nothing was handed to the workspace"
    );
}

/// A record an older store wrote, whose first change is its schema stamp committed with no
/// message, is handed to its workspace whole on the next send, every change of it claimed: the
/// stamp, a root change with no deps that only sets the schema version, as the record's
/// genesis. No change is rewritten and no id changes.
#[test]
fn a_record_whose_first_change_is_an_unnamed_stamp_is_handed_to_its_workspace_whole() {
    let documents: Rc<dyn durable_store::DurableStore> =
        Rc::new(durable_store::MemoryStore::default());
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    // The record as the store wrote it before a record's first change named itself: its layout
    // version, put on the root by the store that created it, and nothing else.
    let stamp = {
        use automerge::transaction::Transactable as _;
        let mut store = collaboration::record::save::workspace_document(
            &documents,
            APP,
            &route.cluster_id.to_string(),
        )
        .unwrap();
        let mut doc = store.new_document();
        doc.put(automerge::ROOT, "tilo:schema_version", 1_u64)
            .unwrap();
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
    let workspace = workspace_copy(&sent);
    assert!(
        automerge::ReadDoc::get_change_by_hash(&workspace, &stamp).is_some(),
        "the stamp, unchanged, reached the workspace"
    );
    assert_eq!(
        automerge::ReadDoc::get_missing_deps(&workspace, &[]),
        Vec::new(),
        "the workspace holds the whole record"
    );
    for request in sent.borrow().iter() {
        if let CollabRequest::Document { mutation, .. } = request {
            let claims =
                collaboration::app_surface::claims_of(APP, mutation).unwrap_or_else(|error| {
                    panic!("every change the workspace is handed is claimed: {error}")
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

const MAIL: &str = "so.tilo.lang.mail";

/// The shipped Mail package, built with the packages it uses.
fn mail() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/mail");
    let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
}

/// Apply the search index deltas the instance published to the session's index, over its
/// certified route, as the session's index applies them.
fn published_index(index: &tilo_desktop_search::content_index::AppContentIndex, sent: &Sent) {
    for request in sent.borrow().iter() {
        if let CollabRequest::PublishSearchIndex { delta_json } = request {
            index
                .publish("room", "so.tilo.lang.mail", "", delta_json)
                .expect("the session takes every delta");
        }
    }
}

#[test]
fn a_mails_message_is_found_by_a_word_of_its_body_through_the_system_index() {
    use tilo_desktop_search::content_index::AppContentIndex;
    use tilo_desktop_search::indexed::IndexedContent;
    use tilo_desktop_search::model::{Source as _, Target};

    // A live Mail instance, its room filled by the package's own scenario, runs its frames.
    let (mut host, mut window, sent, _inbox, _route) = running_app("ada", MAIL, mail());
    host.instances_mut()
        .next()
        .expect("the instance runs")
        .stage("a busy inbox");
    settle(&mut host, &mut window);

    // What it published to the session's one index, as the session keeps it.
    let index = std::sync::Arc::new(AppContentIndex::in_memory());
    published_index(&index, &sent);
    assert!(
        !index.contents().visible.is_empty(),
        "the Mail instance published its conversations and messages"
    );

    // Search reads that index through its production source: a word that appears only inside a
    // message's body (not its subject, its sender or its conversation's snippet) finds the
    // message, with no window running a search of its own.
    let content = IndexedContent::new(std::sync::Arc::clone(&index), ToOwned::to_owned, "");
    let found = content.query("widths");
    assert_eq!(
        found.len(),
        1,
        "a word only a message's body holds finds that one message: {found:?}"
    );
    assert_eq!(
        found[0].subtitle.as_deref(),
        Some("Message"),
        "the hit is the message, by its type"
    );

    // A hit opens Mail at the resource: scoping to Mail keeps it, scoping to another app drops
    // it. `InvokeTarget::app` names the app that published it.
    let app_of = |hit: &tilo_desktop_search::model::Hit| -> Option<String> {
        match &hit.target {
            Target::Invoke(target) => Some(target.app.clone()),
            _ => None,
        }
    };
    assert_eq!(
        app_of(&found[0]).as_deref(),
        Some(MAIL),
        "the hit opens Mail at its message"
    );
    let in_app = |needle: &str, app: &str| {
        content
            .query(needle)
            .into_iter()
            .filter(|hit| app_of(hit).as_deref() == Some(app))
            .count()
    };
    assert_eq!(
        in_app("widths", MAIL),
        1,
        "scoped to Mail: the hit is there"
    );
    assert_eq!(
        in_app("widths", "so.tilo.lang.notes"),
        0,
        "scoped to another app: it is not"
    );
}

/// The text the form the window shows holds for the field `key`, or none while no form is open
/// or the form has no such field.
fn form_text(window: &Window, key: &str) -> Option<String> {
    let form = window.form.as_ref()?;
    form.fields
        .iter()
        .find(|(field, _)| field == key)
        .map(|(_, text)| text.clone())
}

/// The person typed `text` into the field `field` of the form the window shows.
fn type_into(window: &mut Window, field: &str, text: &str) {
    let form = window
        .form
        .as_ref()
        .expect("a form is open to type into")
        .id;
    window.send(&[ToApp::FormEdit {
        form,
        field: field.to_owned(),
        text: text.to_owned(),
    }]);
}

/// The person submitted the form the window shows.
fn submit_form(window: &mut Window) {
    let form = window.form.as_ref().expect("a form is open to submit").id;
    window.send(&[ToApp::FormSubmit { form }]);
}

/// The tasks in the root `hosted` runs, as the runtime reads them.
fn tasks_of(hosted: &Hosted) -> Vec<Value> {
    let instance = hosted.instance();
    let host = instance.host().expect("the runtime");
    let root = host.root_of(TASKS).expect("the tasks root");
    match host.runtime().read(&root, "items").expect("the items") {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

/// When the task `task` is reminded of, as its reminder reads.
fn reminder_of(hosted: &Hosted, task: &Value) -> String {
    let Value::Ref(task) = task else {
        panic!("a task is a resource: {task:?}")
    };
    let instance = hosted.instance();
    let host = instance.host().expect("the runtime");
    match host
        .runtime()
        .read(&task.target, "remind")
        .expect("its reminder")
    {
        Value::Opt(Some(at)) => match *at {
            Value::Text(at) => at,
            other => panic!("a moment: {other:?}"),
        },
        other => panic!("a reminder: {other:?}"),
    }
}

const TASKS: &str = "so.tilo.lang.tasks";

/// The node of the form's tree at `key`.
fn form_node<'a>(window: &'a Window, key: &str) -> Option<&'a patch::Node> {
    fn at<'a>(node: &'a patch::Node, key: &str) -> Option<&'a patch::Node> {
        if node.key == key {
            return Some(node);
        }
        node.children.iter().find_map(|child| at(child, key))
    }
    at(&window.form.as_ref()?.tree, key)
}

/// The choices a `choice` node offers, as the value a pick holds and the words it reads as.
fn choices_of(node: &patch::Node) -> Vec<(String, String)> {
    let Some(patch::Value::List(cases)) = node.props.get("cases") else {
        panic!("a choice with cases: {node:?}");
    };
    cases
        .iter()
        .map(|case| {
            let patch::Value::List(parts) = case else {
                panic!("a case: {case:?}");
            };
            match parts.as_slice() {
                [patch::Value::Text(value), patch::Value::Text(label), ..] => {
                    (value.clone(), label.clone())
                }
                other => panic!("a case's value and label: {other:?}"),
            }
        })
        .collect()
}

/// The palette runs "New task", which still needs the title: its form asks for it, and the
/// person types it. Returns the task the form made.
fn a_task_typed_into_the_palettes_form(host: &mut TilolangHost, window: &mut Window) -> Value {
    // The palette takes "New task", which still needs the title: its form asks for it.
    let new_task = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New task")
        .expect("the tasks publish the action that makes one")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: new_task,
        argument: None,
    }]);
    settle(host, window);
    assert_eq!(
        window.form.as_ref().map(|form| form.title.as_str()),
        Some("New task…"),
        "the form stands for the action, asking what it was not given"
    );
    type_into(window, "form~title", "Call Ada");
    settle(host, window);
    assert_eq!(form_text(window, "form~title").as_deref(), Some("Call Ada"));
    submit_form(window);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "the form closes once the action runs: {:?}",
        window.form.as_ref().map(|form| form.error.clone())
    );
    tasks_of(host.instances().next().expect("the instance runs"))
        .first()
        .cloned()
        .expect("the task it made")
}

/// "Remind me…" asks when, the moments a person names offered to pick from: the person picks
/// one, and the pick answers the form by itself.
fn a_reminder_picked_from_the_forms_choices(
    host: &mut TilolangHost,
    window: &mut Window,
    task: &Value,
) {
    let remind = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "Remind me…")
        .expect("the task offers its reminder")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: remind,
        argument: None,
    }]);
    settle(host, window);
    assert_eq!(
        window.form.as_ref().map(|form| form.title.as_str()),
        Some("Remind me…"),
        "the reminder's form asks when"
    );
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(""),
        "it starts with no moment, the task having no reminder"
    );
    assert!(
        !window.form.as_ref().expect("the form").ready,
        "a moment is required: nothing reads yet, so submit waits"
    );
    assert!(
        !form_node(window, "form~when~field")
            .expect("the moment's field")
            .props
            .contains_key("label"),
        "a form of one question says what it asks in its title"
    );
    let choices = choices_of(form_node(window, "form~when~quick").expect("the moments offered"));
    assert!(
        choices.iter().all(|(_, label)| label != "None"),
        "a required moment offers no way to leave it unanswered: {choices:?}"
    );
    let (moment, _) = choices
        .iter()
        .find(|(_, label)| label == "Tomorrow morning")
        .unwrap_or_else(|| panic!("tomorrow morning is offered: {choices:?}"));
    assert_eq!(
        window.form.as_ref().expect("the form").answers,
        [form_node(window, "form~when~quick")
            .expect("its choices")
            .key
            .clone()],
        "picking a moment answers the form by itself"
    );
    type_into(window, "form~when~quick", moment);
    settle(host, window);
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(moment.as_str()),
        "the field reads as the moment the pick names"
    );
    submit_form(window);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "the form closes once the action runs: {:?}",
        window.form.as_ref().map(|form| form.error.clone())
    );
    assert_eq!(
        &reminder_of(host.instances().next().expect("the instance runs"), task),
        moment,
        "the task is reminded at the moment picked"
    );
}

/// The palette runs an action whose arguments it does not give, and the form asks for them by
/// their types: a task is added by typing its title, and its reminder set by picking the moment
/// it names, both through the invocation the palette sends for any command
/// (forms-and-diagrams.md §1).
#[test]
fn the_palette_asks_the_form_for_a_title_and_a_moment() {
    let (app, bundle) = shipped(&shipped_dir().join("tasks"));
    assert_eq!(app, TASKS);
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
    settle(&mut host, &mut window);
    let task = a_task_typed_into_the_palettes_form(&mut host, &mut window);
    a_reminder_picked_from_the_forms_choices(&mut host, &mut window, &task);
}
