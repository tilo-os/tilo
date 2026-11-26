//! A tilolang app process runs an app's instance without the toolkit: it drives a window in another
//! end of a patch connection, writes what the person did there to the workspace's store on the
//! instance's own connection, takes it back on the person's Undo, publishes the app's actions on
//! that connection and runs what the broker delivers there as its invoker's step. An instance ends
//! when its window closes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::action_directory::{
    ActionDirectoryPage, ActionDirectoryQuery, ActionDirectoryResolution,
};
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollaborationManager, PeerTransport,
    RouteSpec,
};
use runtime_portal::process_link::Shows;
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Level, ToApp, ToUi, Transport as _};
use tilolang_process::TilolangProcess;
use tilolang_process::connection::Connection;
use tilolang_process::hosted::{Hosted, Spec};
use tilolang_process::phrases::Phrases;
use tilolang_process::testing::{Broker, Bus, Directory, Inbox, Sent};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::Value;

/// A check the project's host approved, run for the invocation the broker served.
#[path = "process/approved_checks.rs"]
mod approved_checks;
/// A session's chat, its actions called through the dispatch.
#[path = "process/chat.rs"]
mod chat;
/// An app's calls of another app's action, through the dispatch.
#[path = "process/children.rs"]
mod children;
/// A job outlives its window and its process, and its end reaches the app.
#[path = "process/jobs.rs"]
mod jobs;
/// An agent reads a window as text, calls an action it names, and reads the change.
#[path = "process/outline.rs"]
mod outline;
/// A paste past a mebibyte, kept and undone.
#[path = "process/paste.rs"]
mod paste;
/// An app's route is its own id, whatever its manifest claimed before.
#[path = "process/previously.rs"]
mod previously;
/// The workspace's action registry, read and called by address.
#[path = "process/registry.rs"]
mod registry;
/// What an action returns reaches its invoker over its connection.
#[path = "process/results.rs"]
mod results;
/// Tasks in a hosted window, and Today reading Tasks' resources from the workspace's store.
#[path = "process/tasks.rs"]
mod tasks;
#[path = "process/wakes.rs"]
mod wakes;

const APP: &str = "so.tilo.lang.counter";

/// The seed a package's first compile draws its ids from: its text's FNV-1a hash, so every test
/// that compiles the same text draws the same ids. A process names one surface per app, and
/// many tests of one app share it; ids that differed per compile left the last compile's surface
/// standing over the others' writes.
fn seed(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// [`tilolang::compile_package`], its new ids drawn from `source` rather than the system's
/// randomness: the same text compiles to the same ids wherever a test builds it.
fn compile(
    source: &str,
    ids: &tilolang::IdsLock,
    imports: &[tilolang::PackageMeta],
) -> Result<tilolang::Compiled, Vec<tilolang::Diagnostic>> {
    tilolang::compile_package_with(
        source,
        ids,
        imports,
        &mut tilolang::SeededEntropy(seed(source)),
    )
}

fn counter() -> Bundle {
    let lock =
        tilolang::IdsLock::parse(include_str!("../../../../apps/tl/counter/app.ids")).unwrap();
    let compiled = compile(
        include_str!("../../../../apps/tl/counter/app.tl"),
        &lock,
        &[],
    )
    .unwrap_or_else(|_| panic!("the counter compiles"));
    Bundle::new(&compiled, [])
}

/// The window, as the test drives it from the far end of the patch connection.
struct Window {
    transport: ChannelTransport,
    root: Option<patch::Node>,
    /// The tree of the level beside the root: the item the window shows, and the resource it is.
    detail: Option<patch::Node>,
    detail_target: Option<patch::ResourceRef>,
    /// The item the window shows beside its list: what a press showed in place, as the
    /// window heard it from the instance.
    in_place: Option<patch::ResourceRef>,
    /// The instance asked the window to come forward.
    activated: bool,
    /// What the instance says each bound control holds, by key: its type is what a window
    /// reads to know how a field takes typing.
    bound: BTreeMap<String, patch::Value>,
    /// The form the instance shows for the action waiting for its arguments, as the window
    /// last had it.
    form: Option<Box<patch::Form>>,
    /// The rows the window holds of each virtual list, by the list's key: what the app answered,
    /// with the rows it rendered before kept where it does not answer them again.
    rows: BTreeMap<String, Vec<patch::Node>>,
    /// How many of the window's presses the instance said ran.
    took: u64,
    /// The patches taken in.
    patched: usize,
    /// What the instance told the person, in its words.
    notices: Vec<patch::Words>,
}

impl Window {
    /// A window at the other end of `transport`, shown nothing yet.
    fn over(transport: ChannelTransport) -> Self {
        Self {
            transport,
            root: None,
            detail: None,
            detail_target: None,
            in_place: None,
            activated: false,
            bound: BTreeMap::new(),
            form: None,
            rows: BTreeMap::new(),
            took: 0,
            patched: 0,
            notices: Vec::new(),
        }
    }

    /// Take in what the instance sent: the root's tree, kept up to date with its patches.
    fn take(&mut self) {
        while let Some(batch) = self.transport.recv().unwrap() {
            for message in patch::decode_batch::<ToUi>(&batch).unwrap() {
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
                        self.patched = self.patched.saturating_add(patches.len());
                        let root = self.root.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(root).expect("the patch applies");
                        }
                    }
                    ToUi::Open {
                        level: Level::Detail,
                        target,
                        tree,
                        ..
                    } => {
                        self.detail_target = Some(target);
                        self.detail = Some(tree);
                    }
                    ToUi::Patch {
                        level: Level::Detail,
                        patches,
                    } => {
                        let detail = self.detail.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(detail).expect("the patch applies");
                        }
                    }
                    ToUi::Front {
                        shows: Some((_, resource)),
                        ..
                    } => self.in_place = Some(resource),
                    ToUi::Activate => self.activated = true,
                    ToUi::Bound { key, value, .. } => {
                        self.bound.insert(key, value);
                    }
                    ToUi::Form(form) => self.form = form,
                    // The rows the app answered for a list: an answer names the list's keys from
                    // `start` on and renders those among them the window does not show yet, so
                    // the rows the window holds stay and the answered ones take their place.
                    ToUi::Rows {
                        list,
                        start,
                        keys,
                        rows,
                        ..
                    } => {
                        let stored = self.rows.entry(list).or_default();
                        let mut held: BTreeMap<String, patch::Node> =
                            stored.drain(..).map(|row| (row.key.clone(), row)).collect();
                        for row in rows {
                            held.insert(row.key.clone(), row);
                        }
                        let mut ordered: Vec<patch::Node> = Vec::with_capacity(held.len());
                        if start == 0 {
                            // The answered keys are the list's head, in the order it holds them.
                            for key in keys {
                                if let Some(row) = held.remove(&key) {
                                    ordered.push(row);
                                }
                            }
                        }
                        ordered.extend(held.into_values());
                        *stored = ordered;
                    }
                    // The rows of a list that changed after they were sent: a rendered row
                    // showing something else, or the list read anew.
                    ToUi::RowsChanged { list, changes, .. } => {
                        let stored = self.rows.entry(list).or_default();
                        for change in changes {
                            match change {
                                patch::RowChange::Changed { row } => {
                                    match stored.iter_mut().find(|shown| shown.key == row.key) {
                                        Some(shown) => *shown = *row,
                                        None => stored.push(*row),
                                    }
                                }
                                patch::RowChange::Reset => stored.clear(),
                                patch::RowChange::Inserted { .. }
                                | patch::RowChange::Removed { .. }
                                | patch::RowChange::Moved { .. } => {}
                            }
                        }
                    }
                    ToUi::Took { fired } => self.took = fired,
                    ToUi::Notice { words, .. } => self.notices.push(words),
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

    /// Ask for the rows of the virtual list `list` in `tree` from `start` to `end`, as the list
    /// showing them does: the app answers with them ([`ToUi::Rows`]).
    fn request_rows(&mut self, level: Level, tree: &patch::Node, start: u32, end: u32) {
        let list = tree
            .of_kind("ui.VirtualList")
            .first()
            .expect("the tree shows a list")
            .key
            .clone();
        self.send(&[ToApp::Rows {
            level,
            list,
            start,
            end,
            open: None,
            menu: None,
        }]);
    }

    /// The rows of the virtual list `list` as the app last sent them.
    fn rows_of(&self, list: &str) -> Vec<patch::Node> {
        self.rows.get(list).cloned().unwrap_or_default()
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

/// Written when a host's wake is: work off the frame has something for its loop.
static WOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn wake() -> tilolang_tilo::background::Waker {
    Arc::new(|| WOKEN.store(true, std::sync::atomic::Ordering::SeqCst))
}

/// A host with one counter instance on a broker the test plays, and its window.
fn running(person: &str) -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    running_app(person, APP, counter())
}

/// [`running`] with an instance of `bundle`, the package `app`.
fn running_app(
    person: &str,
    app: &str,
    bundle: Bundle,
) -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    running_with(person, app, bundle, Rc::default())
}

/// [`running_app`], the broker holding the instance's action directory in `directory`.
fn running_with(
    person: &str,
    app: &str,
    bundle: Bundle,
    directory: Rc<RefCell<Directory>>,
) -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    running_on(person, app, bundle, directory, &Rc::default())
}

/// [`running_with`], the workspace `bus` is its store and its members: an instance the test runs
/// beside another's writes the store both read.
fn running_on(
    person: &str,
    app: &str,
    bundle: Bundle,
    directory: Rc<RefCell<Directory>>,
    bus: &Rc<Bus>,
) -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    let route = RouteSpec::app(app, uuid::Uuid::new_v4());
    let mut host = TilolangProcess::new(wake());
    let (window, sent, inbox) = instance_on(
        &mut host,
        spec(1, person, &route),
        bundle,
        &route,
        directory,
        bus,
    );
    (host, window, sent, inbox, route)
}

/// What the instance `number` for `person` on `route` is.
fn spec(number: u64, person: &str, route: &RouteSpec) -> Spec {
    Spec::new(
        route.app_id.clone(),
        route.cluster_id.to_string(),
        number,
        person,
    )
}

/// The instance `attributes` names, of `bundle`, in `host` on `route`, over the workspace `bus` is
/// its store of: its window, what its connection sent and what the broker holds for it. The broker
/// keeps the actions it learns in `directory`.
fn instance_on(
    host: &mut TilolangProcess,
    attributes: Spec,
    bundle: Bundle,
    route: &RouteSpec,
    directory: Rc<RefCell<Directory>>,
    bus: &Rc<Bus>,
) -> (Window, Sent, Inbox) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let broker = Broker::on(bus, route.clone(), &attributes.person).with_directory(directory);
    let (sent, inbox) = (broker.sent(), broker.inbox());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: Some(attributes.person.clone()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    // The store is the workspace's, over the connection: every command waits for the broker's
    // answer (`testing::Workspace`).
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::new(
        attributes,
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(bundle),
        &tilolang_process::hosted::Kept::default(),
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window::over(ChannelTransport::new(theirs).unwrap());
    (window, sent, inbox)
}

/// How long a host may take to be done with what it owes before the test says so: a failure, not
/// a wait — nothing is settled by it.
const SETTLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run the host's loop without its wait: a frame whenever it owes one, until it owes nothing — no
/// instance being opened, no change being applied, and no window with a frame coming. Every
/// frame's patches are taken in `windows`, as a window that draws them does, and `carry` is what
/// the test's broker does between frames.
///
/// Nothing is awaited by letting time pass: the host says whether it owes work, whether it is
/// opening an instance, and what it reads off the frame wakes it when it is ready.
fn settle_all(
    host: &mut TilolangProcess,
    windows: &mut [&mut Window],
    mut carry: impl FnMut(&mut TilolangProcess) -> bool,
) {
    let began = std::time::Instant::now();
    loop {
        let mut changed = host.turn();
        for window in &mut *windows {
            window.take();
        }
        changed |= carry(host);
        let woken = WOKEN.swap(false, std::sync::atomic::Ordering::SeqCst);
        if !changed && !woken && !host.owes_work() {
            return;
        }
        assert!(
            began.elapsed() < SETTLE_LIMIT,
            "the host owes work after {}s: {} opening, {} running; owed {:?}, changed {changed}, woken {woken}",
            began.elapsed().as_secs(),
            host.opening(),
            host.instances().count(),
            host.owed(),
        );
        if !changed {
            // Nothing moved: what the host owes is work off its frame, which wakes it.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// [`settle_all`] of one window, with no broker of its own.
fn settle(host: &mut TilolangProcess, window: &mut Window) {
    settle_all(host, &mut [window], |_| false);
}

/// The store command a request carries, when it carries one.
fn command_of(request: &CollabRequest) -> Option<collab_types::store::wire::Command> {
    match request {
        CollabRequest::ResourceCommand { command, .. } => serde_json::from_slice(command).ok(),
        _ => None,
    }
}

/// How many writes the instance's connection made of the workspace's store.
fn writes_sent(sent: &Sent) -> usize {
    sent.borrow()
        .iter()
        .filter(|request| command_of(request).is_some_and(|command| command.writes()))
        .count()
}

/// The stamp of the write `command` carries, or of the last write in its batch.
fn stamp_of(
    command: &collab_types::store::wire::Command,
) -> Option<&tilo_resource_store::admit::Stamp> {
    use collab_types::store::wire::Command;
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Edit { stamp, .. }
        | Command::Undo { stamp, .. } => Some(stamp),
        Command::Batch(commands) => commands.iter().rev().find_map(stamp_of),
        _ => None,
    }
}

/// Who the last write the instance's connection made names as its invoker, as the store's stamp
/// carries it.
fn last_writer(sent: &Sent) -> Option<String> {
    sent.borrow().iter().rev().find_map(|request| {
        let command = command_of(request).filter(collab_types::store::wire::Command::writes)?;
        Some(stamp_of(&command)?.invoker.clone())
    })
}

/// Who a write of `person`'s own is stamped as, in the store's stamp.
fn writer(person: &str) -> String {
    serde_json::to_string(&tilolang_tilo::runtime::Actor::Member(
        tilolang_tilo::runtime::ids::MemberId::person(person),
    ))
    .expect("an actor writes")
}

tilo_test_env::scenario! {
    fn what_a_window_or_the_broker_asks_runs_as_a_step_that_undo_takes_back;

    phase a_press_in_the_window_is_a_step_on_the_instances_connection_and_undo_takes_it_back;
    {
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
        // the window sends the invocation, and the instance runs it and writes it to the workspace's
        // store as their step.
        let before = writes_sent(&sent);
        press_increment(&mut host, &mut window);
        let hosted = host.instances().next().unwrap();
        assert_eq!(value_of(hosted), 1, "the press reached the runtime");
        assert!(
            writes_sent(&sent) > before,
            "the change went out on the connection"
        );
        assert_eq!(
            last_writer(&sent).as_deref(),
            Some(writer("ada").as_str()),
            "it is ada's step"
        );
        assert!(
            hosted.store().last_written().is_some(),
            "the store holds a step ada can undo"
        );

        // Undo from the window takes it back, as a write of its own sent on the connection.
        let before = writes_sent(&sent);
        window.send(&[ToApp::Undo]);
        settle(&mut host, &mut window);
        let hosted = host.instances().next().unwrap();
        assert_eq!(value_of(hosted), 0, "undo took the press back");
        assert!(
            writes_sent(&sent) > before,
            "the undo went out on the connection"
        );

        // Redo from the window brings it back, the same way.
        let before = writes_sent(&sent);
        window.send(&[ToApp::Redo]);
        settle(&mut host, &mut window);
        assert_eq!(
            value_of(host.instances().next().unwrap()),
            1,
            "redo brought the press back"
        );
        assert!(
            writes_sent(&sent) > before,
            "the redo went out on the connection"
        );
    }

    phase a_chosen_press_shows_and_is_told_in_the_frame_that_ran_it;
    // A press of an action the window chose (its bar, its palette) shows in the frame that ran it:
    // what it changed and that it ran reach the window together, so the window takes them in with
    // one frame rather than one for what it made and another for its having run.
    {
        let (mut host, mut window, _sent, _inbox, _route) = running("ada");
        settle(&mut host, &mut window);
        let address = host
            .instances()
            .next()
            .unwrap()
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
        let patched = window.patched;
        host.turn();
        window.take();
        assert_eq!(
            value_of(host.instances().next().unwrap()),
            1,
            "the press ran"
        );
        assert!(window.patched > patched, "what it changed was sent");
        assert_eq!(window.took, 1, "and that it ran, in the same frame");
    }

    phase an_edit_another_device_made_shows_in_a_hosted_window;
    // What grace did in her window on another device is written to the workspace's store; the
    // broker tells ada's connection which records moved, and ada's window shows what grace wrote.
    {
        let bus = Rc::new(Bus::default());
        let (mut host, mut window, _sent, _inbox, _route) =
            running_on("ada", APP, counter(), Rc::default(), &bus);
        settle(&mut host, &mut window);
        assert_eq!(value_of(host.instances().next().unwrap()), 0);

        // Grace presses + in her window, on the same workspace: her write is the store's.
        let (mut elsewhere, mut graces, _sent, _inbox, _route) =
            running_on("grace", APP, counter(), Rc::default(), &bus);
        settle(&mut elsewhere, &mut graces);
        press_increment(&mut elsewhere, &mut graces);
        assert_eq!(value_of(elsewhere.instances().next().unwrap()), 1);

        // The broker tells ada's connection which records grace's write moved, as it does when
        // another device wrote them; ada's window reads them again.
        settle(&mut host, &mut window);

        assert_eq!(
            value_of(host.instances().next().unwrap()),
            1,
            "grace's press shows in ada's window"
        );
    }

    phase what_the_broker_delivers_runs_as_its_invokers_step;
    {
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
        let invocation = tilo_actions::InvocationId::new();
        inbox.borrow_mut().push_back(CollabEvent::Invocation {
            id: 7,
            invocation,
            resource: increment.resource.clone(),
            action: increment.action,
            argument: None,
            actor: tilo_actions::MemberId::person("grace").into(),
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
            last_writer(&sent).as_deref(),
            Some(writer("grace").as_str()),
            "the step is grace's, who invoked it"
        );
    }
}

/// The person at `window` presses the counter's +: the instance runs the action it publishes.
fn press_increment(host: &mut TilolangProcess, window: &mut Window) {
    let address = host
        .instances()
        .next()
        .unwrap()
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
    settle(host, window);
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
        actor: tilo_actions::MemberId::person(actor).into(),
        context: None,
    });
}

tilo_test_env::scenario! {
    fn the_instance_answers_what_every_app_is_asked_on_its_connection;

    phase the_instance_publishes_and_answers_the_history_verbs_every_app_does;
    {
        let (mut process, mut window, sent, inbox, _route) = running("ada");
        settle(&mut process, &mut window);
        let hosted = process.instances().next().unwrap();
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
        let (undo, redo) = (find("Undo"), find("Redo"));
        // History is offered with them, as every app's is.
        find("History");
        let increment = hosted
            .published()
            .iter()
            .find(|action| action.action_label == "Increment")
            .unwrap()
            .address
            .clone();

        // Grace increments through the broker, then undoes through the broker: her own step.
        deliver(&inbox, 1, &increment, "grace");
        settle(&mut process, &mut window);
        assert_eq!(value_of(process.instances().next().unwrap()), 1);
        assert_eq!(
            last_writer(&sent).as_deref(),
            Some(writer("grace").as_str()),
            "the count is grace's, who invoked it"
        );
        deliver(&inbox, 2, &undo, "grace");
        settle(&mut process, &mut window);
        let hosted = process.instances().next().unwrap();
        assert_eq!(value_of(hosted), 0, "grace's undo took her step back");
        match answered(&sent, 2) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                let value = output.value.expect("undo says what it took back");
                assert_eq!(value["undone"], 1, "{value}");
            }
            other => panic!("undo was answered {other:?}"),
        }

        // Ada's one step is the counter she created. Grace counted in it, but took that back, so
        // nothing of anyone's holds the counter any more: ada's Undo takes it away whole.
        let root = process
            .instances()
            .next()
            .unwrap()
            .instance()
            .host()
            .expect("the runtime")
            .root_of(APP)
            .expect("the counter's root");
        deliver(&inbox, 3, &undo, "ada");
        settle(&mut process, &mut window);
        match answered(&sent, 3) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                let value = output.value.expect("undo says what it took back");
                assert_eq!(value["undone"], 1, "{value}");
            }
            other => panic!("ada's undo is never refused: {other:?}"),
        }
        // Read what the store holds: asking the instance for the root would make a new one in its
        // place.
        assert!(
            !process
                .instances()
                .next()
                .unwrap()
                .store()
                .contains(&root),
            "ada's undo took the counter away"
        );

        // Grace's redo brings back what ada's undo took away: the counter, with nothing counted in
        // it.
        deliver(&inbox, 4, &redo, "grace");
        settle(&mut process, &mut window);
        assert!(
            process
                .instances()
                .next()
                .unwrap()
                .store()
                .contains(&root),
            "the counter is back"
        );
        assert_eq!(
            value_of(process.instances().next().unwrap()),
            0,
            "what was taken back is back, and what grace took back stays back"
        );
        match answered(&sent, 4) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                let value = output.value.expect("redo says what it brought back");
                assert_eq!(value["redone"], 1, "{value}");
            }
            other => panic!("grace's redo was answered {other:?}"),
        }
    }

    phase the_instance_answers_read_and_show_this_window_as_every_app_does;
    {
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
                let value = output.value.expect("read answers what the document holds");
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

    phase a_published_view_opens_through_its_own_action;
    // A view other than its type's default is opened by the action published with it: the
    // counter's `controls`, invoked as the broker delivers it, shows the counter through that
    // view, its two buttons without the count.
    {
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

    phase the_instance_answers_every_handoff_step_the_shell_asks;
    {
        use collaboration::handoff::{HandoffReport, HandoffRequest};
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
        settle(&mut host, &mut window);
        let reports = reports(&sent);
        assert!(
            reports
                .iter()
                .any(|report| matches!(report, HandoffReport::Done { token: 7, .. })),
            "a checkpoint is done: {reports:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn an_instance_opens_without_holding_up_others_and_ends_with_its_window;

    phase an_instance_ends_when_its_window_closes;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running("ada");
        settle(&mut host, &mut window);
        assert!(!host.is_empty());
        drop(window);
        host.turn();
        assert!(host.is_empty(), "the instance ended with its window");
    }

    phase a_slow_open_does_not_hold_up_another_instances_frame;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running("ada");
        settle(&mut host, &mut window);
        let before = shown(&window);

        // A second instance whose workspace has not answered its join yet.
        let answer = Rc::new(std::cell::Cell::new(false));
        let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
        let mut manager =
            CollaborationManager::with_peer_transport(SlowBroker::new(route.clone(), Rc::clone(&answer)));
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: None,
            resource_type: None,
            handle: Some("grace".to_owned()),
        }));
        let (ours, theirs) = Channel::pair().unwrap();
        host.open(
            Spec::new(APP, route.cluster_id.to_string(), 2, "grace"),
            Arc::new(counter()),
            manager,
            ours,
        );
        let mut second = Window::over(ChannelTransport::new(theirs).unwrap());

        // The first instance's window presses +: its next frame comes at once, while the second
        // instance still waits for its workspace.
        let (key, event) = window.increment();
        window.send(&[ToApp::Fire {
            level: Level::Root,
            key,
            event,
            value: None,
            gesture: None,
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
}

/// A broker that answers the join only once the test lets it: an instance whose workspace is slow
/// to answer. Its store answers the instance's resource commands as the workspace's does once it
/// runs.
struct SlowBroker {
    route: RouteSpec,
    answer: Rc<std::cell::Cell<bool>>,
    asked: bool,
    inbox: Inbox,
    workspace: Rc<tilolang_process::testing::Workspace>,
}

impl SlowBroker {
    /// A connection to `route`, whose join is answered once `answer` is set.
    fn new(route: RouteSpec, answer: Rc<std::cell::Cell<bool>>) -> Self {
        Self {
            route,
            answer,
            asked: false,
            inbox: Inbox::default(),
            workspace: Rc::default(),
        }
    }
}

impl PeerTransport for SlowBroker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match request {
            CollabRequest::Hello { .. } => self.asked = true,
            CollabRequest::ResourceCommand {
                request_id,
                command,
            } => {
                let result = serde_json::from_slice(&command)
                    .map_err(|error| error.to_string())
                    .and_then(|command| self.workspace.command(command, "ada"))
                    .and_then(|reply| {
                        serde_json::to_vec(&reply).map_err(|error| error.to_string())
                    });
                self.inbox
                    .borrow_mut()
                    .push_back(CollabEvent::ResourceCommandResult { request_id, result });
            }
            _ => {}
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        (self.asked && self.answer.get()).then(|| {
            self.asked = false;
            Ok(self.route.clone())
        })
    }

    /// The other documents of the workspace are joined too, so a connection's routes open.
    fn join_route(
        &mut self,
        _: RouteSpec,
        _: collab_types::document::ResourceType,
    ) -> Result<(), String> {
        Ok(())
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

/// Ask the instance's directory `query`, as the broker does, and take its answer.
fn page(
    host: &mut TilolangProcess,
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

tilo_test_env::scenario! {
    fn the_instances_action_directory_is_browsed_by_the_api_and_agents;

    phase the_instance_offers_an_action_directory_the_api_and_agents_browse;
    {
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

    phase an_agent_lists_the_apps_root_document_and_its_actions;
    // The app's root document is listed at the directory's top level to an agent as to a person: its
    // `open` changes only the caller's own window, so it discloses the document under the caller's
    // policy rather than keeping it, and every action on it, the publisher's alone. Its level lists
    // the actions an agent takes there.
    {
        let directory = Rc::new(RefCell::new(Directory::default()));
        let (mut host, mut window, _sent, _inbox, _route) =
            running_with("ada", APP, counter(), Rc::clone(&directory));
        settle(&mut host, &mut window);
        let top = page(&mut host, &mut window, &directory, query());
        let prefix = format!("tl:{APP}:root-");
        let root = top
            .rows
            .iter()
            .find(|row| row.resource.starts_with(&prefix) && row.action.is_none())
            .unwrap_or_else(|| panic!("the root document is at the top: {:?}", top.rows))
            .clone();
        let visibility = root
            .visibility
            .as_ref()
            .expect("a listed resource names its visibility");
        assert!(
            visibility.contract.discloses_resource(),
            "the root document is disclosed only to its publisher through `{}`",
            visibility.action
        );
        let level = page(
            &mut host,
            &mut window,
            &directory,
            ActionDirectoryQuery {
                parent_resource: root.resource.clone(),
                ..query()
            },
        );
        // The broker refuses a page whose action row names another contract than its descriptor's.
        for row in level.rows.iter().filter(|row| row.action.is_some()) {
            let descriptor = row
                .descriptor
                .as_ref()
                .expect("an action row carries its action");
            let visibility = row
                .visibility
                .as_ref()
                .expect("an action row names its visibility");
            assert_eq!(visibility.contract, descriptor.contract, "{row:?}");
        }
        for action in ["open", "increment", "decrement"] {
            assert!(
                level.rows.iter().any(|row| {
                    row.resource == root.resource && row.action.as_deref() == Some(action)
                }),
                "{action} is on the root's level: {:?}",
                level.rows
            );
        }
    }
}

const NOTES: &str = "so.tilo.notes";

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
        &mut tilolang::SeededEntropy(seed(&dir.to_string_lossy())),
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
        styles: BTreeMap::new(),
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
    /// How many documents publication looked at while the keys were typed.
    visited: u64,
    /// How many times it read every document in full while the keys were typed.
    swept: u64,
}

/// How many records the instance's publication has looked at so far.
fn visited(host: &TilolangProcess) -> u64 {
    host.instances()
        .next()
        .expect("the app runs")
        .instance()
        .visited_documents()
}

/// How many times the instance's publication has read every document in full so far.
fn swept(host: &TilolangProcess) -> u64 {
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

/// The event that puts `shown` into the field `input` of the page of `level` showing `target`: a
/// rich field is edited whole, as the toolkit sends what the person typed into it, a text field
/// by the splice each key is.
fn key_edit(
    rich: bool,
    level: Level,
    target: &patch::ResourceRef,
    input: &str,
    seq: usize,
    at: u32,
    shown: &str,
    key: char,
) -> ToApp {
    if rich {
        ToApp::EditRich {
            level,
            target: target.clone(),
            key: input.to_owned(),
            rich: typed_rich(shown),
            gesture: None,
        }
    } else {
        ToApp::Splice {
            level,
            target: target.clone(),
            key: input.to_owned(),
            seq: (seq as u64).saturating_add(1),
            at,
            delete: 0,
            insert: key.to_string(),
        }
    }
}

/// The window's own "New note": the press that makes a note and shows it beside the list.
fn press_new_note(window: &mut Window) {
    let button = node_with(
        window.root.as_ref().expect("the notebook is shown"),
        "label",
        "New note",
    )
    .expect("the notebook offers a new note")
    .clone();
    let event = button
        .events
        .keys()
        .next()
        .expect("the button fires")
        .clone();
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key: button.key,
        event,
        value: None,
        gesture: None,
    }]);
}

/// A notebook of `held` notes, an instance of `notes`, the first of them open in the window as
/// the person made it; what the instance did while the person typed `typed` into the open note's
/// field whose placeholder is `field`, a key at a time, each followed by the host's frames until
/// it owes none, as its loop runs them.
fn typing(notes: &Bundle, held: usize, field: &str, typed: &str) -> Typed {
    let directory = Rc::new(RefCell::new(Directory::default()));
    let (mut host, mut window, sent, inbox, _route) =
        running_with("ada", NOTES, notes.clone(), Rc::clone(&directory));
    settle(&mut host, &mut window);
    // The person starts a note from the empty notebook: it opens beside the list.
    press_new_note(&mut window);
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
    // The note the press made shows beside the tree, its page the window's own: the field is
    // in the tree's level, as the window draws it.
    let field_node = node_with(
        window.root.as_ref().expect("the notebook is shown"),
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
    let target = window
        .in_place
        .clone()
        .expect("the press showed the note beside the list");
    let writes = writes_sent(&sent);
    let (mut built, mut relabeled) = (0, 0);
    let mut seen: u64 = 0;
    let mut sweeps: u64 = 0;
    let mut shown = String::new();
    for (seq, key) in typed.chars().enumerate() {
        let at = u32::try_from(shown.len()).unwrap();
        shown.push(key);
        let edit = key_edit(rich, Level::Root, &target, &input, seq, at, &shown, key);
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
        writes_sent(&sent) > writes,
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
    // One build of the package, for the four notebooks typed in here.
    let notes = notes();
    // A new note's title is empty, so its body's first text is its label until it has a
    // title: each key of the body publishes that label alone, with one note or with two
    // hundred, never the whole notebook's surface.
    let mut held_by = Vec::new();
    for held in [1, 200] {
        let body = typing(&notes, held, "Write something", "draft");
        assert_eq!(
            (body.built, body.relabeled),
            (0, 5),
            "typing a body published the surface (whole, labels) with {held} notes"
        );
        held_by.push((held, body.visited));
        assert_eq!(
            body.swept, 0,
            "typing a body read every document in full {} times with {held} notes",
            body.swept
        );
    }
    // A key republishes the note it typed into, and no other document the notebook holds: what a
    // turn costs is the same with one note as with two hundred.
    assert!(
        held_by[0].1 >= 5 && held_by[0].1 <= 10,
        "five keys looked at {} documents",
        held_by[0].1
    );
    assert_eq!(
        held_by[0].1, held_by[1].1,
        "five keys looked at {} documents with one note, {} with 200",
        held_by[0].1, held_by[1].1
    );
    // A note's title is its label: each key publishes that label alone, the same work with one
    // note as with two hundred, and never the whole surface.
    let mut held_by = Vec::new();
    for held in [1, 200] {
        let title = typing(&notes, held, "Untitled", "abc");
        held_by.push((held, title.visited));
        assert_eq!(
            title.swept, 0,
            "typing a title read every document in full {} times with {held} notes",
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
        "three keys looked at {} documents",
        held_by[0].1
    );
    assert_eq!(
        held_by[0].1, held_by[1].1,
        "three keys looked at {} documents with one note, {} with 200",
        held_by[0].1, held_by[1].1
    );
}

/// The title of the note `target` names, as the instance's runtime reads it.
fn title_of(host: &TilolangProcess, target: &patch::ResourceRef) -> String {
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
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let first = window
        .in_place
        .clone()
        .expect("the first note shows beside the list");
    let title = node_with(window.root.as_ref().unwrap(), "placeholder", "Untitled")
        .unwrap_or_else(|| panic!("DEBUGDUMP {:#?}", window.root))
        .key
        .clone();
    // The person typed into the first note's title; the window sent it just as their next
    // press made a second note, which the instance shows by the time the typing arrives.
    press_new_note(&mut window);
    window.send(&[ToApp::Splice {
        level: Level::Root,
        target: first.clone(),
        key: title,
        seq: 1,
        at: 0,
        delete: 0,
        insert: "Plan ".to_owned(),
    }]);
    settle(&mut host, &mut window);
    let second = window
        .in_place
        .clone()
        .expect("the second note shows beside the list");
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

/// Two members, by the hex of their user root keys: the workspace's owner, and a viewer.
const ADA: &str = "abababababababababababababababababababababababababababababababab";
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// A host running `bundle`'s app `app` with no window for `person` ([`Shows::Nothing`]), on a
/// broker the test plays: what its connection sent, what the broker holds for it, and its route.
/// `access` is what the person may do in the app, as the session hands it with the instance.
fn running_with_no_view(
    person: &str,
    app: &str,
    bundle: Bundle,
    access: Option<tilo_action_policy::MemberAccess>,
) -> (
    TilolangProcess,
    Sent,
    Inbox,
    Rc<tilolang_process::testing::Workspace>,
) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(app, uuid::Uuid::new_v4());
    // The broker answers the connection's reads as its person, as the session's does.
    let broker = Broker::on(&Rc::default(), route.clone(), person);
    let (sent, inbox, workspace) = (broker.sent(), broker.inbox(), broker.workspace());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let mut host = TilolangProcess::new(wake());
    let mut spec = Spec::new(app, route.cluster_id.to_string(), 1, person).showing(Shows::Nothing);
    if let Some(access) = access {
        spec = spec.access(access);
    }
    let hosted = Hosted::new(
        spec,
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(bundle),
        &tilolang_process::hosted::Kept::default(),
        Box::new(tilolang_process::hosted::NoView),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    host.add(hosted, None);
    (host, sent, inbox, workspace)
}

fn settle_with_no_view(host: &mut TilolangProcess) {
    let mut windows: [&mut Window; 0] = [];
    settle_all(host, &mut windows, |_| false);
}

/// An agent acting for `person`, as the broker admits one: the member the step is delivered as,
/// and the context naming the agent and who it acts for.
fn agent_step(
    agent: &str,
    person: &str,
) -> (
    tilo_actions::Actor,
    Option<Box<tilo_actions::ActionContext>>,
) {
    let invoker = tilo_actions::ActionInvoker::agent(agent, person).acting_for(person);
    let by = invoker
        .acting
        .by
        .member_id()
        .cloned()
        .expect("an agent is a member");
    let context = tilo_actions::ActionContext {
        invoker: Some(invoker),
        ..tilo_actions::ActionContext::default()
    };
    (tilo_actions::Actor::Member(by), Some(Box::new(context)))
}

/// Deliver invocation `id` of `address` to the instance through `inbox`, as the broker does for
/// the invoker `actor` names.
fn deliver_as(
    inbox: &Inbox,
    id: u64,
    invocation: tilo_actions::InvocationId,
    address: &tilo_actions::ActionAddress,
    actor: tilo_actions::Actor,
    context: Option<Box<tilo_actions::ActionContext>>,
) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation,
        resource: address.resource.clone(),
        action: address.action.clone(),
        argument: None,
        actor,
        context,
    });
}

/// The `label` action the instance running in `host` publishes.
fn published_as(host: &TilolangProcess, label: &str) -> tilo_actions::ActionAddress {
    host.instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == label)
        .unwrap_or_else(|| panic!("the instance publishes {label:?}"))
        .address
        .clone()
}

tilo_test_env::scenario! {
    fn an_instance_that_shows_nothing_runs_the_apps_actions_for_whoever_invokes_them;

    phase a_tasks_action_with_no_window_is_applied_as_its_invocers_step;
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        let (mut host, sent, inbox, workspace) = running_with_no_view("ada", &app, bundle, None);
        settle_with_no_view(&mut host);
        // An instance that shows nothing has no window to bring forward: it offers no such verb.
        assert!(
            host.instances()
                .next()
                .expect("the instance runs")
                .published()
                .iter()
                .all(|action| action.action_label != "Show this window"),
            "an instance with no window offers no window verb"
        );
        let new_task = published_as(&host, "New task");
        let before = workspace.written().len();
        let (actor, context) = agent_step("scribe", "ada");
        let invocation = tilo_actions::InvocationId::new();
        deliver_as(&inbox, 1, invocation, &new_task, actor, context);
        settle_with_no_view(&mut host);
        let answer = answered(&sent, 1).expect("the broker hears how the step ended");
        assert!(
            matches!(answer, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "the agent's step ran: {answer:?}"
        );
        // What it wrote reached the workspace, and says whose step it is: the agent that
        // invoked it, and the invocation the broker admitted.
        let written = workspace.written();
        let step = written.get(before..).unwrap_or_default();
        assert!(!step.is_empty(), "what the step wrote was handed to the workspace");
        for stamp in step {
            assert!(stamp.invoker.contains("scribe"), "the change is the agent's step: {stamp:?}");
            assert_eq!(stamp.invocation, invocation.0.to_string(), "{stamp:?}");
        }
    }

    phase a_member_whose_access_denies_it_is_refused_and_nothing_is_written;
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        let workspace = uuid::Uuid::new_v4();
        let mut policy = tilo_action_policy::WorkspacePolicy::new(workspace.to_string(), ADA);
        policy.members.insert(BO.to_owned(), tilo_action_policy::Role::Viewer);
        let access = tilo_action_policy::MemberAccess::person(policy, ADA);
        let (mut host, sent, inbox, workspace) = running_with_no_view("ada", &app, bundle, Some(access));
        settle_with_no_view(&mut host);
        let new_task = published_as(&host, "New task");
        let before = workspace.written().len();
        // Bo is a viewer and Tasks lets one make no task: the step is refused, and the document
        // holds nothing of it.
        let (actor, context) = (tilo_actions::Actor::Member(tilo_actions::MemberId::person(BO)), None);
        deliver_as(
            &inbox,
            1,
            tilo_actions::InvocationId::new(),
            &new_task,
            actor,
            context,
        );
        settle_with_no_view(&mut host);
        let answer = answered(&sent, 1).expect("the broker hears the refusal");
        let said = format!("{answer:?}");
        assert!(
            said.contains("not allowed") || said.contains("permission_denied"),
            "Bo is refused, with why: {said}"
        );
        assert_eq!(
            workspace.written().len(),
            before,
            "the refused step wrote nothing in the workspace"
        );
    }

    phase a_cancelled_invocation_writes_nothing;
    {
        let (app, bundle) = shipped(&shipped_dir().join("tasks"));
        let (mut host, _, inbox, workspace) = running_with_no_view("ada", &app, bundle, None);
        settle_with_no_view(&mut host);
        let new_task = published_as(&host, "New task");
        let before = workspace.written().len();
        // The caller stopped waiting before the frame that would run it: both reach the instance
        // at once, and the invocation never runs.
        let (actor, context) = agent_step("scribe", "ada");
        deliver_as(
            &inbox,
            1,
            tilo_actions::InvocationId::new(),
            &new_task,
            actor,
            context,
        );
        inbox
            .borrow_mut()
            .push_back(CollabEvent::InvocationCancellation { id: 1 });
        settle_with_no_view(&mut host);
        assert_eq!(
            workspace.written().len(),
            before,
            "the cancelled step wrote nothing in the workspace"
        );
    }
}

/// A package whose window writes when something was fetched, as Mail's does: the clock the
/// person reads, from the moment the app keeps.
const CLOCKED: &str = "so.tilo.lang.clocked";
/// The text of the clocked package: a log whose one moment its window writes as a clock.
const CLOCKED_SOURCE: &str = r#"app so.tilo.lang.clocked "Clocked" (icon: .hash, category: .utility)

/// Something read at a moment.
type Log {
    key: Text merge register
    /// When it was read, as the clock the person read it at.
    at: Time = Time("1970-01-01T00:00:00Z")
}

root Log

default view log for Log

/// The window: when the log was read, as the person reads a clock.
view log(l: Log) {
    std.page(l) {
        section("Fetched") {
            ui.Text("{l.at.clock()}")
        }
    }
}

/// A log read at 10:41.
scenario "fetched" {
    let l = root Log
    l.at = Time("2026-06-30T10:41:00Z")
}
"#;

/// The clocked package, compiled from [`CLOCKED_SOURCE`].
fn clocked() -> Bundle {
    let compiled = compile(CLOCKED_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the clocked package compiles: {diagnostics:?}"));
    Bundle::new(&compiled, [])
}

const MAIL: &str = "so.tilo.mail";

/// The shipped Mail package, built with the packages it uses.
fn mail() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/mail");
    let platform = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
    let provided = tilolang::source::apps_of(&platform);
    let dirs = tilolang::source::dependency_order_with(&dir, &[], &provided)
        .expect("the packages resolve");
    let built = tilolang::source::compile_dirs(
        &dirs,
        &platform,
        &mut tilolang::SeededEntropy(seed(&dir.to_string_lossy())),
    )
    .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
}

/// Publish what search holds of the app the instance runs, read from the workspace's store as
/// the device's daemon reads it ([`tilolang_tilo::search::whole`]): the entries of every resource
/// of the app that holds text, the test's index keeping them under the app's one slot.
fn published_index(index: &tilo_workspace_search::AppContentIndex, host: &Host) {
    let published = tilolang_tilo::search::publish_whole(
        tilo_resource::search_index::Publisher::default(),
        tilo_resource::search_index::IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        tilolang_tilo::search::whole(host),
    );
    for delta in &published.deltas {
        index
            .publish("workspace", MAIL, "", delta)
            .expect("the session takes every delta");
    }
}

#[test]
fn a_mails_message_is_found_by_a_word_of_its_body_through_the_system_index() {
    use tilo_desktop_search::indexed::IndexedContent;
    use tilo_desktop_search::model::{Source as _, Target};
    use tilo_workspace_search::{AppContentIndex, IndexSearch};

    // A live Mail instance, its room filled by the package's own scenario, runs its frames.
    let (mut host, mut window, sent, _inbox, _route) = running_app("ada", MAIL, mail());
    host.instances_mut()
        .next()
        .expect("the instance runs")
        .stage("a busy inbox");
    settle(&mut host, &mut window);

    // The window publishes nothing to search: the device's daemon owns the app's entries.
    assert!(
        !sent
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::PublishSearchIndex { .. })),
        "an open window sends no search entry"
    );

    // What the daemon reads of the app's store, as the session's one index keeps it.
    let index = std::sync::Arc::new(AppContentIndex::in_memory());
    {
        let running = host.instances_mut().next().expect("the instance runs");
        let instance_host = running.instance().host().expect("the instance's store");
        published_index(&index, &instance_host);
    }
    assert!(
        !index.contents().visible.is_empty(),
        "search holds the Mail app's conversations and messages"
    );

    // Search reads that index through its production source: a word a message's body holds
    // finds the message, with no window running a search of its own. Its conversation holds it
    // too, since the conversation's snippet is the start of that body, so the two records the
    // word is in are the two hits.
    let search = IndexSearch::new(
        std::sync::Arc::clone(&index),
        tilo_workspace_search::deciding(|_, _| true),
    );
    let content = IndexedContent::new(
        std::sync::Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        ToOwned::to_owned,
        "",
        std::sync::Arc::new(|| {}),
    );
    let found = content.query("widths");
    let mut kinds: Vec<&str> = found
        .iter()
        .filter_map(|hit| hit.subtitle.as_deref())
        .collect();
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        ["Message", "Thread"],
        "the word is found in the message that says it, and in its conversation: {found:?}"
    );
    assert_eq!(
        found[0].subtitle.as_deref(),
        Some("Message"),
        "the message leads, by its type"
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
        2,
        "scoped to Mail: the message and its conversation are there"
    );
    assert_eq!(
        in_app("widths", "so.tilo.notes"),
        0,
        "scoped to another app: it is not"
    );
}

/// The person's formats with the clock `time`.
fn with_clock(time: tilo_formats::TimeStyle) -> tilo_formats::RegionFormats {
    tilo_formats::RegionFormats {
        time,
        ..tilo_formats::RegionFormats::default()
    }
}

tilo_test_env::scenario! {
    fn a_hosted_window_writes_dates_and_times_as_the_person_set_them;

    phase a_hosted_window_writes_the_clock_the_person_reads;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", CLOCKED, clocked());
        assert!(host.set_formats(with_clock(tilo_formats::TimeStyle::H12)));
        host.instances_mut()
            .next()
            .expect("the instance runs")
            .stage("fetched");
        settle(&mut host, &mut window);
        let written = shown(&window);
        assert!(
            written.contains("10:41 AM"),
            "a 12-hour clock reads the moment the app wrote: {written}"
        );

        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", CLOCKED, clocked());
        assert!(host.set_formats(with_clock(tilo_formats::TimeStyle::H24)));
        host.instances_mut()
            .next()
            .expect("the instance runs")
            .stage("fetched");
        settle(&mut host, &mut window);
        let written = shown(&window);
        assert!(
            written.contains("10:41") && !written.contains("10:41 AM"),
            "a 24-hour clock reads it without the half of the day: {written}"
        );
    }

    phase a_clock_the_person_changes_is_written_again_in_the_window;
    {
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", CLOCKED, clocked());
        assert!(host.set_formats(with_clock(tilo_formats::TimeStyle::H24)));
        host.instances_mut()
            .next()
            .expect("the instance runs")
            .stage("fetched");
        settle(&mut host, &mut window);
        let before = shown(&window);
        assert!(
            before.contains("10:41"),
            "the 24-hour clock first: {before}"
        );

        // The person picks the 12-hour clock in Settings: the host hears it on its api, and what
        // the instance already wrote reads in it.
        assert!(
            host.set_formats(with_clock(tilo_formats::TimeStyle::H12)),
            "another clock is a change"
        );
        settle(&mut host, &mut window);
        let after = shown(&window);
        assert!(
            after.contains("10:41 AM"),
            "the notice is written again in the clock the person chose: {after}"
        );
    }

    phase a_week_start_the_person_changes_is_written_again_in_the_window;
    // A person who starts their week on Sunday in Settings sees the week their window shows move
    // with them: the session says so on the host's api, and the instance writes the day again.
    {
        let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", WEEKS, weeks());
        host.instances_mut()
            .next()
            .expect("the instance runs")
            .stage("this week");
        settle(&mut host, &mut window);
        let monday = shown(&window);
        assert!(
            monday.contains("Oct 26"),
            "until the person says otherwise, Thursday the 29th stands in the week that began on \
         Monday: {monday}"
        );

        // Sunday in Settings, said on the host's api as the session says it: the same day stands in
        // the week that began the Sunday before it, and the window shows that week.
        let (session, api) = runtime_portal::process_link::pair().expect("an API");
        runtime_portal::process_link::Packet::send(
            &runtime_portal::process_link::ToProcess::Locale(runtime_portal::process_link::Locale {
                formats: with_first_day(tilo_formats::FirstDayOfWeek::Sunday),
                language: String::new(),
            }),
            &session,
        )
        .expect("the formats cross the API");
        let Some(runtime_portal::process_link::ToProcess::Locale(locale)) =
            <runtime_portal::process_link::ToProcess as runtime_portal::process_link::Packet>::receive(&api)
                .expect("the API carries them")
        else {
            panic!("the API carries the person's formats as formats");
        };
        assert!(host.set_locale(&locale), "another first day is a change");
        settle(&mut host, &mut window);
        let sunday = shown(&window);
        assert!(
            sunday.contains("Oct 25"),
            "the week the person starts on Sunday is the one their window shows: {sunday}"
        );
    }
}

tilo_test_env::scenario! {
    fn the_workspaces_content_a_device_reads_reaches_the_windows_root;

    phase a_refusal_the_device_reads_shows_in_the_window_and_clears_when_the_limit_is_raised;
    // A window reads the workspace's owners' limits and the content a limit refused
    // (`workspace.content`). The device reads them from its workspaces and says them on the API;
    // the window shows them where it stands, and the refusal goes when the limit is raised.
    {
        let (mut host, mut window, _sent, _inbox, route) =
            running_app("ada", CONTENT, contented());
        settle(&mut host, &mut window);
        let empty = shown(&window);
        assert!(
            empty.contains("0 bytes, 0 refused"),
            "a workspace whose content was all kept reads as none refused: {empty}"
        );

        // The device reads a refusal for the file a message carries, and says it on the API as
        // it says it to a process running the workspace's windows.
        let refused = tilolang_tilo::content::WorkspaceContent {
            file_bytes: 1_000,
            member_bytes: 5_000,
            hourly_bytes: 5_000,
            refusals: vec![tilolang_tilo::content::RefusedContent {
                record: "so.tilo.chat/".to_owned(),
                hash: "ab".repeat(32),
                bytes: 1_200,
                limit: "file".to_owned(),
                allowed: 1_000,
                member: "ada".to_owned(),
                at: 7,
                raise: "owners".to_owned(),
            }],
            host_person: None,
        };
        let (session, api) = runtime_portal::process_link::pair().expect("an API");
        runtime_portal::process_link::Packet::send(
            &runtime_portal::process_link::ToProcess::Content(
                runtime_portal::process_link::WorkspaceContent {
                    workspace: route.cluster_id.to_string(),
                    content: serde_json::to_string(&refused).expect("a reading encodes"),
                },
            ),
            &session,
        )
        .expect("the reading crosses the API");
        let packet =
            <runtime_portal::process_link::ToProcess as runtime_portal::process_link::Packet>::receive(
                &api,
            )
            .expect("the API carries it");
        let Some(runtime_portal::process_link::ToProcess::Content(content)) = packet else {
            panic!("the API carries the workspace's content as content");
        };
        let reading: tilolang_tilo::content::WorkspaceContent =
            serde_json::from_str(&content.content).expect("the reading reads");
        assert!(
            host.set_content(&content.workspace, Some(&reading)),
            "another reading is a change"
        );
        settle(&mut host, &mut window);
        let refused = shown(&window);
        assert!(
            refused.contains("1000 bytes, 1 refused"),
            "the window shows the limit and the refusal: {refused}"
        );

        // The owners raise the limit and the file is kept: the refusal no longer stands.
        let raised = tilolang_tilo::content::WorkspaceContent {
            file_bytes: 5_000,
            ..tilolang_tilo::content::WorkspaceContent::default()
        };
        assert!(host.set_content(&route.cluster_id.to_string(), Some(&raised)));
        settle(&mut host, &mut window);
        let cleared = shown(&window);
        assert!(
            cleared.contains("5000 bytes, 0 refused"),
            "the refusal goes with the limit that made it: {cleared}"
        );
    }
}

/// The package whose window reads the workspace's content root.
const CONTENT: &str = "so.tilo.lang.content";

/// The text of the content package: a note's window says the limits on what members post in the
/// workspace and how much of it a limit refused.
const CONTENT_SOURCE: &str = r#"app so.tilo.lang.content "Content" (icon: .hash, category: .utility)

use tilo.workspace as workspace

/// Something the person reads the workspace's content limits beside.
type Note {
    key: Text merge register
    read: Text = ""
}

root Note

default view note for Note

/// The window: the largest file the workspace's owners let a host keep, and how many refusals
/// stand for the person at this window.
view note(n: Note) {
    std.page(n) {
        section("Kept") {
            ui.Text("{workspace.content.file_bytes} bytes, {workspace.content.refusals.count()} refused")
        }
    }
}

/// A note read before anything was typed.
scenario "empty" {
    let n = root Note
    n.read = "nothing yet"
}
"#;

/// The content package, compiled against the workspace package it reads.
fn contented() -> Bundle {
    let used = tilolang_tilo::platform_apps::platform_packages_of(["tilo.workspace"])
        .expect("the workspace package reads");
    let compiled = compile(CONTENT_SOURCE, &tilolang::IdsLock::new(), &used)
        .unwrap_or_else(|diagnostics| panic!("the content package compiles: {diagnostics:#?}"));
    Bundle::new(&compiled, [])
}

/// A package whose window writes the day the week it shows began on, as the person's region
/// starts its week: the "this week" a view reads.
const WEEKS: &str = "so.tilo.lang.week";

/// The text of the week package: a log whose window writes the day this week began on.
const WEEKS_SOURCE: &str = r#"app so.tilo.lang.week "Week" (icon: .hash, category: .utility)

/// Something the person looks at this week.
type Log {
    key: Text merge register
    /// When it was read.
    at: Date = Date("2026-10-29")
}

root Log

default view log for Log

/// The window: the day this week began on, as the person's region starts a week.
view log(l: Log) {
    std.page(l) {
        section("This week") {
            ui.Text("{l.at.start_of_week()}")
        }
    }
}

/// A log read on the 29th of October.
scenario "this week" {
    let l = root Log
    l.at = Date("2026-10-29")
}
"#;

/// The week package, compiled from [`WEEKS_SOURCE`].
fn weeks() -> Bundle {
    let compiled = compile(WEEKS_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the week package compiles: {diagnostics:?}"));
    Bundle::new(&compiled, [])
}

/// The person's formats with the week starting on `first_day`.
fn with_first_day(first_day: tilo_formats::FirstDayOfWeek) -> tilo_formats::RegionFormats {
    tilo_formats::RegionFormats {
        first_day,
        ..tilo_formats::RegionFormats::default()
    }
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

const TASKS: &str = "so.tilo.tasks";

/// A window a German person reads names another app's change by that app's own name: the names of
/// the apps installed here arrive on the instance's connection with the workspace's people's
/// names, and are what its history reads. The language the names are read in is the process's, so
/// this test reads them in a process of its own.
#[test]
fn a_hosted_window_names_another_app_as_the_names_it_was_sent() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_hosted_window_names_another_app_as_the_names_it_was_sent",
        || {
            use tilolang_tilo::names::Installed;
            use tilolang_tilo::runtime::{Actor, NameHost as _};

            let today = Actor::App("so.tilo.lang.today".into());
            let (mut host, mut window, _sent, inbox, _route) = running("ada");
            settle(&mut host, &mut window);
            assert_eq!(
                Installed.name(&today),
                "Today",
                "the words its id makes, before the session named it"
            );
            collab_types::history::authors::set_names_language("de-DE");
            inbox.borrow_mut().push_back(CollabEvent::Context {
                permissions: Vec::new(),
                apps: vec![collab_types::history::NamedApp {
                    id: "so.tilo.lang.today".to_owned(),
                    name: "Today".to_owned(),
                    names: [("de-DE".to_owned(), "Heute".to_owned())]
                        .into_iter()
                        .collect(),
                }],
            });
            settle(&mut host, &mut window);
            assert_eq!(
                Installed.name(&today),
                "Heute",
                "the app's own name in the window's language"
            );
            assert_eq!(
                Installed.name(&Actor::App("org.example.Editor".into())),
                "Editor",
                "an app installed nowhere near this device reads as the words its id makes"
            );
        },
    );
}

/// The Notes app, and Discussion it is made with: the app a Markdown file opens in.
fn notes_bundle() -> Bundle {
    let discussion_source = include_str!("../../../../apps/tl/discussion/app.tl");
    let discussion = compile(
        discussion_source,
        &tilolang::IdsLock::new(),
        &tilolang_tilo::platform_apps::named_platform_packages(discussion_source)
            .expect("the platform's packages read"),
    )
    .unwrap_or_else(|diagnostics| panic!("the discussion app compiles: {diagnostics:?}"));
    let lock = tilolang::IdsLock::parse(include_str!("../../../../apps/tl/notes/app.ids")).unwrap();
    let notes_source = include_str!("../../../../apps/tl/notes/app.tl");
    let mut imports = tilolang_tilo::platform_apps::named_platform_packages(notes_source)
        .expect("the platform's packages read");
    imports.push(discussion.meta);
    let compiled = compile(notes_source, &lock, &imports)
        .unwrap_or_else(|diagnostics| panic!("the notes app compiles: {diagnostics:?}"));
    Bundle::new(&compiled, [])
}

/// The notes the app's notebook holds, as the titles and the files they came from.
fn notes_made(host: &TilolangProcess) -> Vec<(String, String)> {
    fn text(
        host: &tilolang_tilo::host::Host,
        at: &tilolang_tilo::runtime::ResourceRef,
        field: &str,
    ) -> String {
        match host.runtime().read(at, field).unwrap() {
            Value::Text(text) => text,
            other => panic!("a note's {field} reads as {other:?}"),
        }
    }
    let hosted = host.instances().next().expect("the window's instance runs");
    let live = hosted.instance().host().expect("the instance's host");
    let root = live.root_of(NOTES).unwrap();
    let notes = match live.runtime().read(&root, "notes").unwrap() {
        Value::List(items) => items,
        other => panic!("the notebook's notes read as {other:?}"),
    };
    notes
        .iter()
        .map(|note| {
            let Value::Ref(note) = note else {
                panic!("a note is a reference")
            };
            (
                text(&live, &note.target, "title"),
                text(&live, &note.target, "origin"),
            )
        })
        .collect()
}

/// A window opened with a file runs the action its package declares for that kind of file: the
/// app's process reads the bytes from the content store and hands the action the same `File` a
/// chooser gives it, and what it made opens in the window. A Markdown file opened in Notes becomes
/// the note its first heading names, and keeps the file it came from.
#[test]
fn a_window_opened_with_a_markdown_file_imports_it_into_notes() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let route = RouteSpec::app(NOTES, uuid::Uuid::new_v4());
    // The broker answers the connection's reads as its person, as the session's does.
    let broker = Broker::on(&Rc::default(), route.clone(), "ada");
    let sent = broker.sent();
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some("ada".to_owned()),
    }));
    let _ = manager.poll();
    let content = tilo_content::ContentStore::memory();
    let hash = content.put(b"# Plan\n\n- one\n").unwrap();
    // The content store the session opens for the app in its workspace: what the file the window
    // was opened with is read from, and what its own `file.text()` reads.
    let kept = tilolang_process::hosted::Kept {
        content: Some(content.clone()),
        chooser: None,
        clipboard: None,
        thumbnails: None,
        tools: None,
    };
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let mut host = TilolangProcess::new(wake());
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let mut hosted = Hosted::new(
        Spec::new(NOTES, route.cluster_id.to_string(), 1, "ada"),
        connection,
        (store, content),
        Arc::new(notes_bundle()),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    hosted.open_file(runtime_portal::process_link::OpenedFile {
        hash,
        name: Some("plan.md".into()),
    });
    host.add(hosted, Some(watched));
    let mut window = Window::over(ChannelTransport::new(theirs).unwrap());
    settle(&mut host, &mut window);
    assert_eq!(
        notes_made(&host),
        [("Plan".to_owned(), "plan.md".to_owned())],
        "the file became the note its heading names, keeping the file it came from"
    );
    drop(sent);
}

/// A window opened with a `tube:` link runs the app's `open-link`: the video the link names
/// shows in the window, as a press of its row shows one.
#[test]
fn a_window_opened_with_a_tube_link_shows_the_video_it_names() {
    let (_, tube) = shipped(&shipped_dir().join("tube"));
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", "so.tilo.tube", tube);
    host.instances_mut()
        .next()
        .expect("the instance runs")
        .stage("a subscriptions feed");
    settle(&mut host, &mut window);
    // The root offers the system's `open-link`, which is what a shell delivers a link through
    // when it hands one to the app rather than to a window.
    let open_link = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.address.action == "open-link")
        .expect("the root offers the link it opens");
    assert_eq!(
        open_link.address.resource, "tl:so.tilo.tube:root-thwdswkk",
        "the action a shell delivers a link through is on the root"
    );
    host.instances_mut()
        .next()
        .expect("the instance runs")
        .open_link("tube:watch?v=v-phone".into());
    settle(&mut host, &mut window);
    let shown = window
        .detail_target
        .clone()
        .expect("the window shows the video the link names");
    let at = tilolang_tilo::instance::wire::runtime_resource(&shown).expect("a resource");
    let instance = host
        .instances()
        .next()
        .expect("the instance runs")
        .instance();
    let live = instance.host().expect("the instance's host");
    match live
        .runtime()
        .read(&at, "title")
        .expect("the video's title")
    {
        Value::Text(title) => assert_eq!(title, "The phone that finally lasts a week"),
        other => panic!("a video's title reads as {other:?}"),
    }
}

/// A package whose action opens the file its window was opened with, in the app that opens its
/// kind.
const OPENER: &str = "so.tilo.lang.opener";

/// The opener's source: an action that takes the file it was opened with and opens it again.
const OPENER_SOURCE: &str = r#"app so.tilo.lang.opener "Opener" (icon: .hash, category: .utility)

type Panel {
    said: Text = ""
}

root Panel

default view panel for Panel

view panel(p: Panel) {
    text(p.said)
}

action opened(p: Panel, file: File) uses files (opens: ["text/markdown", .md]) {
    p.said = match File.open(file) {
        .ok => "opened",
        .err(why) => why,
    }
}
"#;

/// The opener's package, compiled from [`OPENER_SOURCE`].
fn opener() -> Bundle {
    let compiled = compile(OPENER_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the opener compiles: {diagnostics:?}"));
    Bundle::new(&compiled, [])
}

/// The session's tools service, answered here over a connection: each request is answered from
/// `answers` in turn and kept in `asked`, as the session answers one for an app.
struct ToolsStub {
    asked: std::sync::Arc<std::sync::Mutex<Vec<runtime_interface::tools::ToolIntent>>>,
    answers: std::sync::Arc<std::sync::Mutex<Vec<runtime_interface::tools::ToolReply>>>,
}

impl ToolsStub {
    /// A stub answering each request with the next of `answers`, in the order given.
    fn new(answers: Vec<runtime_interface::tools::ToolReply>) -> Self {
        Self {
            asked: std::sync::Arc::default(),
            answers: std::sync::Arc::new(std::sync::Mutex::new(
                answers.into_iter().rev().collect(),
            )),
        }
    }

    /// Answer `stream`'s requests until it closes, on a thread of its own: what the session's end
    /// of an app's connection does.
    fn serve(&self, stream: std::os::unix::net::UnixStream) {
        let asked = std::sync::Arc::clone(&self.asked);
        let answers = std::sync::Arc::clone(&self.answers);
        std::thread::spawn(move || {
            use std::io::{BufRead as _, BufReader, Write as _};
            let Ok(mut writing) = stream.try_clone() else {
                return;
            };
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let Ok(request) =
                    serde_json::from_str::<runtime_interface::tools::ToolRequest>(&line)
                else {
                    return;
                };
                asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.intent);
                let reply = answers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop()
                    .unwrap_or_else(|| runtime_interface::tools::ToolReply {
                        accepted: false,
                        message: "the test service has no more answers".into(),
                        ..runtime_interface::tools::ToolReply::default()
                    });
                let Ok(mut encoded) = serde_json::to_vec(&reply) else {
                    return;
                };
                encoded.push(b'\n');
                if writing.write_all(&encoded).is_err() {
                    return;
                }
            }
        });
    }

    /// What it was asked, oldest first.
    fn asked(&self) -> Vec<runtime_interface::tools::ToolIntent> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A listing that offers `app` for a file of the kind asked, or none when `app` is none.
fn listing(app: Option<&str>) -> runtime_interface::tools::ToolReply {
    runtime_interface::tools::ToolReply {
        accepted: true,
        message: "listed".into(),
        documents: app
            .map(|id| runtime_interface::tools::DocumentApp {
                id: id.to_owned(),
                name: id.to_owned(),
                is_default: true,
                icon: None,
                source: Some(runtime_interface::tools::AppKind::Tilo),
                artwork: None,
            })
            .into_iter()
            .collect(),
        ..runtime_interface::tools::ToolReply::default()
    }
}

/// The opener instance, its window opened with `bytes` as `name`, over a tools connection answered
/// by `stub`: the host, its window, and the stub.
fn opening_with(stub: &ToolsStub, bytes: &[u8], name: &str) -> (TilolangProcess, Window) {
    let route = RouteSpec::app(OPENER, uuid::Uuid::new_v4());
    // The broker answers the connection's reads as its person, as the session's does.
    let broker = Broker::on(&Rc::default(), route.clone(), "ada");
    let sent = broker.sent();
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some("ada".to_owned()),
    }));
    let _ = manager.poll();
    let content = tilo_content::ContentStore::memory();
    let hash = content.put(bytes).unwrap();
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    stub.serve(ours);
    let tools = runtime_interface::tools::Service::connect(theirs, route.cluster_id.to_string());
    let kept = tilolang_process::hosted::Kept {
        content: Some(content.clone()),
        chooser: None,
        clipboard: None,
        thumbnails: None,
        tools: Some(std::sync::Arc::new(tools)),
    };
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let mut host = TilolangProcess::new(wake());
    let (window, patches) = Channel::pair().unwrap();
    let watched = window.as_fd().try_clone_to_owned().unwrap();
    let mut hosted = Hosted::new(
        Spec::new(OPENER, route.cluster_id.to_string(), 1, "ada"),
        connection,
        (store, content),
        Arc::new(opener()),
        &kept,
        Box::new(ChannelTransport::new(window).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    hosted.open_file(runtime_portal::process_link::OpenedFile {
        hash,
        name: Some(name.to_owned()),
    });
    host.add(hosted, Some(watched));
    let window = Window::over(ChannelTransport::new(patches).unwrap());
    drop(sent);
    (host, window)
}

/// What the opener's panel says after its action ran.
fn opener_said(host: &TilolangProcess) -> String {
    let instance = host.instances().next().expect("the window's instance runs");
    let live = instance.instance().host().expect("the instance's host");
    let root = live.root_of(OPENER).expect("the opener's root");
    match live.runtime().read(&root, "said").unwrap() {
        Value::Text(said) => said,
        other => panic!("the panel's said reads as {other:?}"),
    }
}

/// `File.open(file)` in an action the window was opened with asks the session which app opens the
/// file's kind, by the content's bytes and its name, and opens it there: the address travels, as
/// it does for a file a window hands another app.
#[test]
fn a_file_an_action_holds_opens_in_the_app_that_opens_its_kind() {
    let stub = ToolsStub::new(vec![
        listing(Some("so.tilo.lang.reader")),
        runtime_interface::tools::ToolReply {
            accepted: true,
            message: "opened".into(),
            ..runtime_interface::tools::ToolReply::default()
        },
    ]);
    let (mut host, mut window) = opening_with(&stub, b"# Plan\n", "plan.md");
    settle(&mut host, &mut window);
    assert_eq!(opener_said(&host), "opened");
    let asked = stub.asked();
    assert_eq!(
        asked.len(),
        2,
        "the kind is asked, then the open: {asked:?}"
    );
    assert!(
        matches!(
            &asked[0],
            runtime_interface::tools::ToolIntent::DocumentApps { path } if path == "file.md"
        ),
        "the kind a Markdown file is, asked by its name: {:?}",
        asked[0]
    );
    match &asked[1] {
        runtime_interface::tools::ToolIntent::OpenContent { name, hash, app } => {
            assert_eq!(name, "plan.md");
            assert_eq!(app, "so.tilo.lang.reader");
            assert_eq!(hash.len(), 64, "the address of the bytes: {hash}");
        }
        other => panic!("the file opens as content in the app that opens its kind: {other:?}"),
    }
}

/// A file no app here opens is a failure the action sees, with what to do about it.
#[test]
fn a_file_no_app_opens_says_so() {
    let stub = ToolsStub::new(vec![listing(None)]);
    let (mut host, mut window) = opening_with(&stub, b"# Plan\n", "plan.md");
    settle(&mut host, &mut window);
    assert_eq!(
        opener_said(&host),
        "nothing on this computer opens `plan.md`",
        "the person reads which file nothing opens"
    );
    assert_eq!(
        stub.asked().len(),
        1,
        "nothing is opened once nothing opens it"
    );
}

/// A session that refuses the open — the app's package did not declare `files.open` — is a failure
/// the action sees, in the session's own words.
#[test]
fn an_open_the_session_refuses_says_why() {
    let refused = "a file is opened by Files, or by an app that declares files.open";
    let stub = ToolsStub::new(vec![
        listing(Some("so.tilo.lang.reader")),
        runtime_interface::tools::ToolReply {
            accepted: false,
            message: refused.into(),
            ..runtime_interface::tools::ToolReply::default()
        },
    ]);
    let (mut host, mut window) = opening_with(&stub, b"# Plan\n", "plan.md");
    settle(&mut host, &mut window);
    assert_eq!(opener_said(&host), refused);
}

/// The note `target` names stands under, as the instance's runtime reads it.
fn parent_of(host: &TilolangProcess, target: &patch::ResourceRef) -> Option<String> {
    let instance = host.instances().next().unwrap().instance();
    let resource = tilolang_tilo::instance::wire::runtime_resource(target).unwrap();
    match instance
        .host()
        .unwrap()
        .runtime()
        .read(&resource, "parent")
        .unwrap()
    {
        Value::Opt(Some(held)) => match *held {
            Value::Ref(note) => Some(note.target.id_string()),
            other => panic!("a parent is a note, not {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("a note's parent is {other:?}"),
    }
}

/// A note an action the person chose makes opens where the window shows it: the window's own
/// "New sub-note" makes a note under the one open, and the window turns to the note it made — its
/// page beside the tree — as a press of the action that made it.
#[test]
fn a_note_an_action_made_opens_where_the_window_shows_it() {
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", NOTES, notes());
    settle(&mut host, &mut window);
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let above = window
        .in_place
        .clone()
        .expect("the first note shows beside the tree");
    // The action the window's bar and palette offer: a note under the note the window shows.
    let address = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New sub-note")
        .expect("New sub-note is published")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle(&mut host, &mut window);
    let made = window
        .in_place
        .clone()
        .expect("the note the action made shows beside the tree");
    assert_ne!(
        made.id, above.id,
        "the window turns to the note the action made"
    );
    assert_eq!(
        parent_of(&host, &made).as_deref(),
        Some(above.id.as_str()),
        "the note made stands under the note that was open"
    );
}

/// The tags the note `note` carries, as the runtime reads them.
fn tags_of(host: &TilolangProcess, note: &patch::ResourceRef) -> Vec<String> {
    let notes = host.instances().next().unwrap().instance();
    let live = notes.host().expect("the runtime");
    let resource = tilolang_tilo::instance::wire::runtime_resource(note).unwrap();
    match live
        .runtime()
        .read(&resource, "tags")
        .expect("the note's tags")
    {
        Value::List(tags) => tags
            .iter()
            .filter_map(|tag| tag.as_text().map(str::to_owned))
            .collect(),
        other => panic!("a note's tags read as {other:?}"),
    }
}

/// A form asking for a required text does not read whole while it is empty: pressing its
/// button before the person typed files nothing, and the words typed after it are the answer.
#[test]
fn a_form_asking_for_a_required_text_takes_it_before_it_answers() {
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", NOTES, notes());
    settle(&mut host, &mut window);
    press_new_note(&mut window);
    settle(&mut host, &mut window);
    let note = window
        .in_place
        .clone()
        .expect("the press showed the note beside the list");
    let address = host
        .instances()
        .next()
        .expect("the runs")
        .published()
        .iter()
        .find(|action| action.action_label == "Tag…")
        .expect("Tag… is published")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle(&mut host, &mut window);
    let form = window.form.as_ref().expect("the action asks for a tag");
    assert!(
        !form.ready,
        "a form asking a required text is not whole yet"
    );
    submit_form(&mut window);
    settle(&mut host, &mut window);
    assert!(
        tags_of(&host, &note).is_empty(),
        "submitting nothing files nothing"
    );
    type_into(&mut window, "form~tag", "travel");
    settle(&mut host, &mut window);
    assert!(
        window.form.as_ref().is_some_and(|form| form.ready),
        "the form reads whole once the words are in it"
    );
    submit_form(&mut window);
    settle(&mut host, &mut window);
    assert_eq!(
        tags_of(&host, &note),
        ["travel"],
        "the words the person typed are the tag"
    );
}

struct HostedAgreementHost {
    host: TilolangProcess,
    window: Window,
}

impl tilolang_tilo::instance::testing::AgreementHost for HostedAgreementHost {
    fn name(&self) -> &'static str {
        "HostPlatform"
    }

    fn set_formats(&mut self, formats: tilo_formats::RegionFormats) {
        let _ = self.host.set_formats(formats);
    }

    fn set_person_language(&mut self, locale: &str) {
        let _ = self
            .host
            .instances_mut()
            .next()
            .expect("the probe runs")
            .speak(locale);
    }

    fn invoke(
        &mut self,
        actor: &tilolang_tilo::runtime::Actor,
        action_id: &str,
        mut args: Vec<Value>,
    ) {
        let hosted = self.host.instances().next().expect("the probe runs");
        let live = hosted.instance().host().expect("the runtime");
        let entry = live
            .root_of(tilolang_tilo::instance::testing::PROBE_APP)
            .expect("the probe root");
        args.insert(0, Value::resource(&entry));
        live.runtime()
            .invoke_action(actor, (action_id, args))
            .expect("the probe action runs");
    }

    fn sync_and_read(&mut self) -> patch::Node {
        settle(&mut self.host, &mut self.window);
        self.window.root.clone().expect("the hosted root is shown")
    }
}

/// A hosted window agrees with every other host on the shared table of dates, times, clocks,
/// week starts and German typed dates.
#[test]
fn a_hosted_window_agrees_with_every_other_host_on_the_shared_table() {
    use tilolang_tilo::instance::testing::{PROBE_APP, assert_platform_agrees, compile_probe};

    let (compiled, actions) = compile_probe();
    let probe_bundle = Bundle::new(&compiled, []);
    let (host, window, _sent, _inbox, _route) = running_app("ada", PROBE_APP, probe_bundle);
    let mut harness = HostedAgreementHost { host, window };
    settle(&mut harness.host, &mut harness.window);
    assert_platform_agrees(&mut harness, &actions);
}

/// A German person launching an English-only hosted app (`Spec::language = "de-DE"`): the window
/// speaks English ("Today"), and a date typed in German words ("Freitag") is read.
#[test]
fn a_german_person_in_an_english_only_hosted_app_sees_english_and_parses_german_dates() {
    use tilolang_tilo::instance::testing::{
        PROBE_APP, change_mark_said, compile_probe, pinned_wall, tree_text, utc_zone,
    };
    use tilolang_tilo::runtime::Actor;
    use tilolang_tilo::runtime::ids::MemberId;

    struct ZoneGuard;
    impl Drop for ZoneGuard {
        fn drop(&mut self) {
            tilo_clock::local::install(None);
        }
    }

    let (compiled, actions) = compile_probe();
    let probe_bundle = Bundle::new(&compiled, []);
    tilo_clock::testing::with_wall(pinned_wall(), || {
        tilo_clock::local::install(Some(utc_zone()));
        let _zone = ZoneGuard;

        let route = RouteSpec::app(PROBE_APP, uuid::Uuid::new_v4());
        let mut host = TilolangProcess::new(wake());
        let attributes = spec(1, "ada", &route).in_language("de-DE");
        let (mut german_window, _sent, _inbox) = instance_on(
            &mut host,
            attributes,
            probe_bundle,
            &route,
            Rc::default(),
            &Rc::default(),
        );
        settle(&mut host, &mut german_window);
        {
            let hosted = host.instances().next().expect("probe runs");
            let live = hosted.instance().host().expect("runtime");
            let entry = live.root_of(PROBE_APP).expect("root");
            let grace = Actor::Member(MemberId::person("grace"));
            let ada = Actor::Member(MemberId::person("ada"));
            // Ada invokes `read_day` before Grace's `set_day` so Grace's write remains the
            // unseen change shown by `ui.ChangeMark(e)`.
            live.runtime()
                .invoke_action(
                    &ada,
                    (
                        actions.read_day.as_str(),
                        vec![Value::resource(&entry), Value::text("Freitag")],
                    ),
                )
                .expect("ada parses Freitag");
            live.runtime()
                .invoke_action(
                    &grace,
                    (
                        actions.set_day.as_str(),
                        vec![Value::resource(&entry), Value::text("2026-06-30")],
                    ),
                )
                .expect("grace sets today");
        }
        settle(&mut host, &mut german_window);
        let root = german_window.root.as_ref().expect("root shown");
        let said = change_mark_said(root).unwrap_or_default();
        assert_eq!(
            said, "moved it to Today",
            "English-only hosted app speaks English for a German person"
        );
        let rendered = tree_text(root);
        assert!(
            rendered.contains("parsed:2026-10-30"),
            "German 'Freitag' is parsed in an English-only hosted app: `{rendered}`"
        );
    });
}

/// Switching language on a translated hosted package moves `words_revision` and republishes
/// action labels in the new language.
#[test]
fn a_hosted_window_advances_words_revision_and_republishes_translated_labels() {
    let (mut notes_host, mut notes_window, _sent, _inbox, _route) =
        running_app("ada", NOTES, notes());
    settle(&mut notes_host, &mut notes_window);
    let rev_en = {
        let hosted = notes_host.instances().next().expect("notes runs");
        assert!(
            hosted
                .published()
                .iter()
                .any(|action| action.action_label == "New note"),
            "notes publishes English labels initially"
        );
        hosted
            .instance()
            .host()
            .expect("notes runtime")
            .runtime()
            .words()
            .revision()
    };
    assert!(
        notes_host
            .instances_mut()
            .next()
            .expect("notes runs")
            .speak("de-DE"),
        "notes ships German, so speaking de-DE switches its language"
    );
    settle(&mut notes_host, &mut notes_window);
    let hosted = notes_host.instances().next().expect("notes runs");
    let rev_de = hosted
        .instance()
        .host()
        .expect("notes runtime")
        .runtime()
        .words()
        .revision();
    assert_ne!(
        rev_de, rev_en,
        "speaking another language advances words_revision"
    );
    let labels: Vec<&str> = hosted
        .published()
        .iter()
        .map(|action| action.action_label.as_str())
        .collect();
    assert!(
        labels.contains(&"Neue Notiz") && !labels.contains(&"New note"),
        "published action labels update to German when words_revision moves: {labels:?}"
    );
}

const SOUNDCHECK: &str = "so.tilo.lang.soundcheck";

/// A package that reads a field of the device's sound and asks the device to mute it: the
/// session publishes both, so the app holds neither.
const SOUNDCHECK_SOURCE: &str = r#"app so.tilo.lang.soundcheck "Sound check" (icon: .hash, category: .utility)

use so.tilo.device as device

type Panel {
    /// How many times the person asked for more.
    presses: Int merge counter
}

root Panel

default view panel for Panel

view panel(p: Panel) {
    std.page(p) {
        ui.Text(if device.sound.muted { "Muted" } else { "On" }, role: .title)
        ui.Button("Mute") {
            on press: device.toggle_muted(device.sound)
        }
        ui.Text("Pressed {p.presses}", numeric: true)
    }
}
"#;

/// The session's published resources as a test plays them: the readings a test hands in, and the
/// changes it was asked for.
#[derive(Default)]
struct Published {
    queued: std::sync::Mutex<BTreeMap<String, std::collections::VecDeque<String>>>,
    wakes: std::sync::Mutex<BTreeMap<String, tilo_wake::WakeHandle>>,
    invoked: std::sync::Mutex<Vec<(String, String)>>,
}

impl Published {
    /// Install this stand-in as where the platform's roots are read, once, and hand it back.
    fn install() -> Arc<Self> {
        static INSTALLED: std::sync::OnceLock<Arc<Published>> = std::sync::OnceLock::new();
        INSTALLED
            .get_or_init(|| {
                let published = Arc::new(Published::default());
                tilolang_tilo::platform_apps::install(Box::new(TestSession(Arc::clone(
                    &published,
                ))));
                published
            })
            .clone()
    }

    /// The session publishes `reading` of `resource`, waking the views that read it.
    fn publish(&self, resource: &str, reading: &str) {
        self.queued
            .lock()
            .unwrap()
            .entry(resource.to_owned())
            .or_default()
            .push_back(reading.to_owned());
        if let Some(wake) = self.wakes.lock().unwrap().get(resource) {
            wake.wake();
        }
    }

    /// The changes asked of the session, as `(resource, action)`.
    fn invoked(&self) -> Vec<(String, String)> {
        self.invoked.lock().unwrap().clone()
    }
}

/// [`Published`] as the source the process reads the platform's roots through.
struct TestSession(Arc<Published>);

impl tilolang_tilo::platform_apps::PublishedSource for TestSession {
    fn follow(
        &self,
        resource: &str,
        wake: tilo_wake::WakeHandle,
    ) -> Result<Box<dyn tilolang_tilo::platform_apps::Reading>, String> {
        self.0
            .wakes
            .lock()
            .unwrap()
            .insert(resource.to_owned(), wake);
        Ok(Box::new(Followed {
            published: Arc::clone(&self.0),
            resource: resource.to_owned(),
        }))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        self.0
            .invoked
            .lock()
            .unwrap()
            .push((resource.to_owned(), action.to_owned()));
        Ok(())
    }
}

/// One followed resource: the readings [`Published`] queued for it.
struct Followed {
    published: Arc<Published>,
    resource: String,
}

impl tilolang_tilo::platform_apps::Reading for Followed {
    fn latest(&self) -> Option<Result<String, String>> {
        self.published
            .queued
            .lock()
            .unwrap()
            .get_mut(&self.resource)?
            .pop_front()
            .map(Ok)
    }
}

/// The button `label` words, as the key it fires from and the event it fires.
fn button_labelled(window: &Window, label: &str) -> (String, String) {
    let button = buttons(window)
        .into_iter()
        .find(|button| {
            button
                .props
                .values()
                .any(|value| format!("{value:?}").contains(label))
        })
        .unwrap_or_else(|| panic!("the window shows a `{label}` button: {}", shown(window)));
    let event = button
        .events
        .keys()
        .next()
        .expect("the button fires")
        .clone();
    (button.key.clone(), event)
}

fn buttons(window: &Window) -> Vec<&patch::Node> {
    fn walk<'a>(node: &'a patch::Node, out: &mut Vec<&'a patch::Node>) {
        if node.kind == "ui.Button" {
            out.push(node);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut found = Vec::new();
    walk(window.root.as_ref().expect("the root is shown"), &mut found);
    found
}

/// A package that reads the device's published sound and whose press is the device's own verb:
/// the session's readings reach its views, a new one runs them again, and the press is asked of
/// the session, not recorded here.
#[test]
fn a_platform_packages_reading_reaches_the_app_and_its_verb_goes_to_the_session() {
    let published = Published::install();
    let platform = tilolang_tilo::platform_apps::platform_packages().unwrap();
    let compiled = compile(SOUNDCHECK_SOURCE, &tilolang::IdsLock::new(), &platform)
        .unwrap_or_else(|diagnostics| panic!("the sound check compiles: {diagnostics:#?}"));
    let (mut host, mut window, _sent, _inbox, _route) =
        running_app("ada", SOUNDCHECK, Bundle::new(&compiled, []));

    // The session publishes the sound muted, and the window that opens reads it.
    published.publish("sound", r#"{"muted":true}"#);
    settle(&mut host, &mut window);
    assert!(
        shown(&window).contains("Muted"),
        "the session's reading renders: {}",
        shown(&window)
    );

    // A second reading runs the view again without the person pressing anything.
    published.publish("sound", r#"{"muted":false}"#);
    settle(&mut host, &mut window);
    assert!(
        shown(&window).contains("On") && !shown(&window).contains("Muted"),
        "the new reading renders: {}",
        shown(&window)
    );

    // The press is the device's verb, asked of the session.
    let (key, event) = button_labelled(&window, "Mute");
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key,
        event,
        value: None,
        gesture: None,
    }]);
    settle(&mut host, &mut window);
    tilo_test_env::wait_until("the mute reached the session", || {
        published.invoked() == [("sound".to_owned(), "toggle-muted".to_owned())]
    });
}

const HISTORY: &str = "so.tilo.lang.history";

/// A package that reads the workspace's activity and marks a change seen: the session publishes
/// both, as it does the device's sound.
const HISTORY_SOURCE: &str = r#"app so.tilo.lang.history "History" (icon: .hash, category: .utility)

use so.tilo.history as activity

type Panel {
    /// What the person marked.
    seen: Int merge counter
}

root Panel

default view panel for Panel

view panel(p: Panel) {
    std.page(p) {
        if let feed = activity.history.feed {
            if let newest = feed.entries.first() {
                ui.Text(newest.summary, role: .title)
            }
        }
        ui.Button("Seen") {
            on press: activity.mark_viewed(activity.history, entries: ["00112233445566778899aabbccddeeff"], later: false)
        }
        ui.Text("Seen {p.seen}", numeric: true)
    }
}
"#;

/// A workspace's activity is a platform package too: its feed reaches the app that reads it, and
/// the app's press is the activity's own verb, asked of the session.
#[test]
fn the_workspaces_activity_reaches_the_app_that_reads_it() {
    let published = Published::install();
    let platform = tilolang_tilo::platform_apps::platform_packages().unwrap();
    let compiled = compile(HISTORY_SOURCE, &tilolang::IdsLock::new(), &platform)
        .unwrap_or_else(|diagnostics| panic!("the history package compiles: {diagnostics:#?}"));
    let (mut host, mut window, _sent, _inbox, _route) =
        running_app("ada", HISTORY, Bundle::new(&compiled, []));

    published.publish(
        "workspace.activity",
        r#"{"feed":{"workspace":"Home","scope":"this-workspace","total":1,"entries":[{"id":"00112233445566778899aabbccddeeff","author":{"name":"Ada","rgb":[200,120,90],"identity":"ada","relation":"reader","on_behalf_of":null,"for_reader":true,"task":null},"kind":"text","at_ms":1700000000000,"summary":"Renamed a note","app":"Notes","resource":null,"group":null,"redacted":false,"refused":[]}],"groups":[],"people":["Ada"],"lenses":[],"pending":false,"mutations":true}}"#,
    );
    settle(&mut host, &mut window);
    assert!(
        shown(&window).contains("Renamed a note"),
        "the activity's reading renders: {}",
        shown(&window)
    );

    let (key, event) = button_labelled(&window, "Seen");
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key,
        event,
        value: None,
        gesture: None,
    }]);
    settle(&mut host, &mut window);
    tilo_test_env::wait_until("marking it seen reached the session", || {
        published
            .invoked()
            .contains(&("workspace.activity".to_owned(), "mark-viewed".to_owned()))
    });
}

const ACTIVITY: &str = "so.tilo.activity";

/// The Activity app itself marks what it showed as seen: a person's press on its own control is
/// the activity's verb, asked of the session with the changes the briefs named.
#[test]
fn the_activity_app_marks_what_it_showed_as_seen() {
    let published = Published::install();
    let platform = tilolang_tilo::platform_apps::platform_packages().unwrap();
    let lock =
        tilolang::IdsLock::parse(include_str!("../../../../apps/tl/activity/app.ids")).unwrap();
    let compiled = compile(
        include_str!("../../../../apps/tl/activity/app.tl"),
        &lock,
        &platform,
    )
    .unwrap_or_else(|diagnostics| panic!("the activity app compiles: {diagnostics:#?}"));
    let (mut host, mut window, _sent, _inbox, _route) =
        running_app("ada", ACTIVITY, Bundle::new(&compiled, []));
    published.publish(
        "workspace.activity",
        r#"{"summary":{"workspace":"Home","away":true,"pending":false,"sources_available":false,"briefs":[{"title":"Tidied the notes","detail":"Renamed three notes.","authors":[],"apps":["Notes"],"resources":[],"changes":3,"actions":[],"first_ms":1700000000000,"last_ms":1700000060000,"entries":["00112233445566778899aabbccddeeff"]}]}}"#,
    );
    settle(&mut host, &mut window);
    assert!(
        shown(&window).contains("Tidied the notes"),
        "the summary renders: {}",
        shown(&window)
    );

    let (key, event) = button_labelled(&window, "Mark as viewed");
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key,
        event,
        value: None,
        gesture: None,
    }]);
    settle(&mut host, &mut window);
    tilo_test_env::wait_until("the briefs' changes are marked seen", || {
        published
            .invoked()
            .contains(&("workspace.activity".to_owned(), "mark-viewed".to_owned()))
    });
}
