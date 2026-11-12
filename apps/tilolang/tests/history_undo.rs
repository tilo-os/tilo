//! One undo, whichever runs the instance: `undo(v)` on a step and `undo(run)` on an agent's
//! run are the record's platform history verb (`collaboration::history_source::verbs`), which
//! both the window in the app's own process (the framework's root over the app's record) and a
//! tilolang host apply to the record. The same scenario runs against each host, so the two
//! cannot drift apart.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test: a failure is a panic"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted as HostedInstance, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::DynValue;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::record::InstanceRecord;
use ui::headless::FrameHarness;
use ui::tilo_actions::{ActionContext, ActionInvoker, ActorKind};

const TASKS: &str = "so.tilo.lang.tasks";
const RUN: &str = "issue-7@8000000000000001";

/// The Tasks package, compiled from source as `tilolang build` compiles it, with the packages
/// it uses.
fn tasks() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl/tasks");
    let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .expect("the packages compile");
    let (package, used) = built.split_last().expect("the package is built last");
    Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
}

/// The id the package gave its action named `name`.
fn action_id(bundle: &Bundle, name: &str) -> String {
    bundle
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the package declares `{name}`"))
        .id
        .clone()
}

/// An agent's run of `issue-7`, acting for ada, as the broker stamps its changes.
fn marked_run() -> (ActionInvoker, ActionContext) {
    (
        ActionInvoker::actor("agent:helper")
            .with_kind(ActorKind::Agent)
            .acting_for("ada"),
        ActionContext::default().with_group(
            RUN,
            "Tidy the tasks",
            ui::tilo_actions::context::TASK_RUN_GROUP,
        ),
    )
}

fn titled(title: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), title.into());
    arguments
}

/// Where an instance runs and how the person's actions and the record's history verbs reach
/// it: the window in the app's own process, or a tilolang host's.
trait Window {
    /// Frames until nothing is left to do.
    fn settle(&mut self);
    /// The app's record: where its history verbs are published.
    fn record(&self) -> String;
    /// The root the package's window shows (`tl:<app>:<id>`).
    fn root(&mut self) -> String;
    /// The id of the action named `name` published on `resource`.
    fn action(&mut self, resource: &str, name: &str) -> String;
    /// Run the published action `action` of `resource` as `invoker`'s step (the person's own
    /// when `None`), within `ambient` when that names a run.
    fn run(
        &mut self,
        invoker: Option<ActionInvoker>,
        ambient: Option<&ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    );
    /// What the record holds, read without changing what the test reads after.
    fn read<R>(&mut self, read: impl FnOnce(&mut ui::tilo_resource::Doc) -> R) -> R;
}

/// The titles of the tasks in the root, in order.
fn titles(doc: &mut ui::tilo_resource::Doc) -> Vec<String> {
    let root = tilolang_tilo::action::resource_ids(doc, TASKS).remove(0);
    let chunks = tilolang_tilo::chunk::Chunks::of(doc.clone());
    let record =
        tilolang_tilo::action::read_resource(&chunks, TASKS, &root).expect("the root reads");
    record
        .schema()
        .fields
        .iter()
        .filter_map(|field| match record.get(&field.id) {
            Some(DynValue::Collection(items)) => Some(items),
            _ => None,
        })
        .flat_map(|items| items.iter())
        .filter_map(|(_, item)| {
            let title = item
                .schema()
                .fields
                .iter()
                .find(|field| field.kind == tilolang_tilo::MergeKind::Text)?;
            match item.get(&title.id) {
                Some(DynValue::Text(title)) => Some(title.clone()),
                _ => None,
            }
        })
        .collect()
}

/// The change id of the person's own step whose arguments name `title`, outside any run.
fn persons_step(doc: &mut ui::tilo_resource::Doc, title: &str) -> String {
    ui::tilo_resource::history::versions(doc)
        .into_iter()
        .find(|version| {
            version.context.as_ref().is_some_and(|context| {
                context.group.is_none()
                    && context.what.as_ref().is_some_and(|did| {
                        format!("{did:?}").contains(&format!("String({title:?})"))
                    })
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "the person's own step naming {title}: {:?}",
                ui::tilo_resource::history::versions(&mut doc.clone())
                    .iter()
                    .filter_map(|version| version.context.as_ref())
                    .map(|context| format!("{:?}", context.what))
                    .collect::<Vec<_>>()
            )
        })
        .hash
        .to_string()
}

/// One undo implementation, both hosts: the person adds a task and takes back exactly that
/// step with `undo(v)`; an agent's run adds three and `undo(run)` takes back all three, leaving
/// the person's own.
fn a_view_s_undo_of_a_step_and_a_run(window: &mut impl Window) {
    window.settle();
    let root = window.root();
    let add = window.action(&root, "new_task");
    window.run(None, None, &root, &add, &titled("Call the landlord"));
    window.settle();
    assert_eq!(window.read(titles), ["Call the landlord"]);

    // `undo(v)` on the person's own step: exactly that step comes back, and nothing else.
    let person = window.read(|doc| persons_step(doc, "Call the landlord"));
    let record = window.record();
    let mut undo_step = serde_json::Map::new();
    undo_step.insert("step".to_owned(), person.into());
    window.run(None, None, &record, "undo", &undo_step);
    window.settle();
    assert_eq!(
        window.read(titles),
        Vec::<String>::new(),
        "`undo(v)` takes back exactly the step the history named"
    );

    // They add one again, and an agent's run adds three: one step of theirs, one run.
    window.run(None, None, &root, &add, &titled("Call the landlord"));
    window.settle();
    let (agent, ambient) = marked_run();
    for title in ["Book the dentist", "Pay the rent", "Water the plants"] {
        window.run(
            Some(agent.clone()),
            Some(&ambient),
            &root,
            &add,
            &titled(title),
        );
    }
    window.settle();
    assert_eq!(window.read(titles).len(), 4);

    // `undo(run)`: every step of the run comes back together, and the person's own step stays.
    let mut undo_run = serde_json::Map::new();
    undo_run.insert("run".to_owned(), RUN.into());
    window.run(None, None, &record, "undo", &undo_run);
    window.settle();
    assert_eq!(
        window.read(titles),
        ["Call the landlord"],
        "the run's three adds are one step: all of them back, the person's own task kept"
    );
}

#[test]
fn undo_named_from_a_view_takes_back_a_step_and_a_run_in_the_in_process_window() {
    a_view_s_undo_of_a_step_and_a_run(&mut InProcess::open());
}

#[test]
fn undo_named_from_a_view_takes_back_a_step_and_a_run_in_a_tilolang_host() {
    a_view_s_undo_of_a_step_and_a_run(&mut Hosted::open());
}

// ----- the window in the app's own process ---------------------------------------------------

/// Ada at the Tasks window the framework runs in this process: the app's record, the window's
/// own views, and the framework's door, as every window of the app has them.
struct InProcess {
    app: ui::framework::testing::HostedApp,
    add: String,
}

impl InProcess {
    fn open() -> Self {
        static DEFINITION: std::sync::OnceLock<&'static ui::application::ApplicationDefinition> =
            std::sync::OnceLock::new();
        let definition = *DEFINITION.get_or_init(|| {
            tilolang_app::install(tasks()).expect("one package in the process");
            Box::leak(Box::new(tilolang_app::definition()))
        });
        Self {
            add: action_id(&tasks(), "new_task"),
            app: ui::framework::testing::HostedApp::open(definition, "ada").expect("the app opens"),
        }
    }
}

impl Window for InProcess {
    fn settle(&mut self) {
        for _ in 0..12 {
            self.app
                .harness()
                .settle_for_capture(ui::time::Duration::from_millis(300));
            self.app.harness().relayout();
        }
    }

    fn record(&self) -> String {
        // The framework publishes every app's history verbs on the app's record, and the record
        // answers to the resource's kind.
        tilolang_app::RECORD_KIND.to_owned()
    }

    fn root(&mut self) -> String {
        let root = self
            .app
            .read(|doc| tilolang_tilo::action::resource_ids(doc, TASKS).remove(0));
        format!("tl:{TASKS}:{root}")
    }

    fn action(&mut self, resource: &str, name: &str) -> String {
        fn flattened<'a>(
            specs: &'a [ui::tilo_actions::ResourceSpec],
            out: &mut Vec<&'a ui::tilo_actions::ResourceSpec>,
        ) {
            for spec in specs {
                out.push(spec);
                flattened(&spec.children, out);
            }
        }
        let specs = self.app.harness().resources();
        let mut all = Vec::new();
        flattened(&specs, &mut all);
        let id = (name == "new_task").then(|| self.add.clone());
        all.iter()
            .find(|spec| spec.id.as_str() == resource)
            .and_then(|spec| {
                spec.actions
                    .iter()
                    .find(|action| id.as_deref().is_some_and(|id| action.id == id))
            })
            .map_or_else(
                || {
                    panic!(
                        "{resource} publishes `{name}`: {:?}",
                        all.iter().map(|spec| spec.id.as_str()).collect::<Vec<_>>()
                    )
                },
                |action| action.id.clone(),
            )
    }

    fn run(
        &mut self,
        invoker: Option<ActionInvoker>,
        ambient: Option<&ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) {
        self.app
            .invoke(invoker, ambient, resource, action, arguments, false)
            .expect("the door runs it");
    }

    fn read<R>(&mut self, read: impl FnOnce(&mut ui::tilo_resource::Doc) -> R) -> R {
        self.app.read(read)
    }
}

// ----- a tilolang host -----------------------------------------------------------------------

/// The broker's end of the instance's connection: what the test delivers, and what the
/// instance answers.
struct Broker {
    route: RouteSpec,
    sent: Rc<RefCell<Vec<CollabRequest>>>,
    inbox: Rc<RefCell<VecDeque<CollabEvent>>>,
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.sent.borrow_mut().push(request);
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        self.inbox.borrow_mut().drain(..).collect()
    }
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        Some(Ok(self.route.clone()))
    }
}

/// Ada at the Tasks window whose instance a tilolang host runs, reached over the broker.
struct Hosted {
    host: TilolangHost,
    window: FrameHarness,
    sent: Rc<RefCell<Vec<CollabRequest>>>,
    inbox: Rc<RefCell<VecDeque<CollabEvent>>>,
    add: String,
    next: u64,
}

impl Hosted {
    fn open() -> Self {
        let bundle = tasks();
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let sent: Rc<RefCell<Vec<CollabRequest>>> = Rc::default();
        let inbox: Rc<RefCell<VecDeque<CollabEvent>>> = Rc::default();
        let mut manager = CollaborationManager::with_peer_transport(Broker {
            route: route.clone(),
            sent: Rc::clone(&sent),
            inbox: Rc::clone(&inbox),
        });
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: None,
            record_kind: None,
            handle: Some("ada".to_owned()),
        }));
        let _ = manager.poll();
        let record =
            InstanceRecord::new(automerge::AutoCommit::new(), manager, route.clone(), "ada");
        let wake: tilolang_tilo::background::Waker = Arc::new(|| {});
        let mut host = TilolangHost::new(None, Arc::clone(&wake));
        let kept = host.kept(TASKS).unwrap();
        let (ours, theirs) = Channel::pair().unwrap();
        let hosted = HostedInstance::with_record(
            Spec::new(TASKS, route.cluster_id.to_string(), 1, "ada"),
            record,
            Arc::new(bundle.clone()),
            &kept,
            Box::new(ChannelTransport::new(ours).unwrap()),
            &Rc::new(Phrases::new("en-US")),
            host.formats(),
            wake,
        );
        host.add(hosted, None);
        let window = FrameHarness::new(
            ui::State::new(),
            Box::new(tilolang_app::hosted::HostedWindow::over(theirs)),
            (1280, 800),
        );
        Self {
            host,
            window,
            sent,
            inbox,
            add: action_id(&bundle, "new_task"),
            next: 0,
        }
    }

    /// Deliver one invocation as the broker does, and wait until the instance answers it.
    fn invoke(
        &mut self,
        invoker: Option<&ActionInvoker>,
        ambient: Option<&ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) {
        self.next = self.next.saturating_add(1);
        let id = self.next;
        self.inbox.borrow_mut().push_back(CollabEvent::Invocation {
            id,
            invocation: ui::tilo_actions::InvocationId::new(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            argument: Some(format!(
                "{}{}",
                ui::tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments.clone())
            )),
            actor: invoker.map_or_else(|| "ada".to_owned(), |invoker| invoker.actor.clone()),
            context: ambient.cloned().map(Box::new),
        });
        for _ in 0..2000 {
            self.host.turn();
            self.window.relayout();
            let answered = self
                .sent
                .borrow()
                .iter()
                .rev()
                .find_map(|request| match request {
                    CollabRequest::UpdateInvocation {
                        id: answered,
                        update,
                    } if *answered == id => Some(update.clone()),
                    _ => None,
                });
            match answered {
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. }) => return,
                Some(other) => panic!("`{action}` on `{resource}` answered {other:?}"),
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        panic!("`{action}` on `{resource}` was never answered");
    }

    /// The published descriptors.
    fn published(&self) -> Vec<(String, String, String)> {
        self.host
            .instances()
            .next()
            .map(|hosted| {
                hosted
                    .published()
                    .iter()
                    .map(|d| {
                        (
                            d.address.resource.clone(),
                            d.address.action.clone(),
                            d.action_label.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Window for Hosted {
    fn settle(&mut self) {
        for _ in 0..40 {
            let changed = self.host.turn();
            self.window.relayout();
            if !changed {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }

    fn record(&self) -> String {
        // A tilolang host publishes the history verbs every app has on the app's own resource.
        tilolang_tilo::publish::package_resource_id(TASKS)
    }

    fn root(&mut self) -> String {
        for _ in 0..400 {
            let root = self
                .published()
                .into_iter()
                .find(|(resource, _, label)| {
                    resource.starts_with(&format!("tl:{TASKS}:")) && label == "New task"
                })
                .map(|(resource, _, _)| resource);
            if let Some(root) = root {
                return root;
            }
            self.host.turn();
            self.window.relayout();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the root is published: {:?}", self.published());
    }

    fn action(&mut self, resource: &str, name: &str) -> String {
        let want = if name == "new_task" {
            self.add.clone()
        } else {
            name.to_owned()
        };
        for _ in 0..400 {
            let found = self
                .published()
                .into_iter()
                .find_map(|(published, action, _)| {
                    (published == resource && action == want).then_some(action)
                });
            if let Some(action) = found {
                return action;
            }
            self.host.turn();
            self.window.relayout();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("{resource} publishes `{name}`: {:?}", self.published());
    }

    fn run(
        &mut self,
        invoker: Option<ActionInvoker>,
        ambient: Option<&ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) {
        self.invoke(invoker.as_ref(), ambient, resource, action, arguments);
    }

    fn read<R>(&mut self, read: impl FnOnce(&mut ui::tilo_resource::Doc) -> R) -> R {
        let hosted = self.host.instances_mut().next().expect("the instance runs");
        read(&mut hosted.record().doc().clone())
    }
}
