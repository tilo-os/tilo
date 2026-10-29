//! Actions a host publishes on the door and runs on its own thread.
//!
//! A [`ResourceApp`](super::ResourceApp) declares its actions on its resource
//! and runs them in its views. A host whose actions act on state it keeps
//! itself, outside any document (the desktop shell's chrome), mounts a
//! [`HostedActions`] as its app's root instead: the widget publishes the
//! resources the host hands its [`HostedActionsLink`], admits each invocation
//! of the link's operation type, and leaves it with the link. The host takes
//! the invocations from there on its own loop, where its state is not
//! borrowed, runs them, and answers each through the link; the app reports
//! the answer to whoever invoked it. So the host's actions reach people,
//! scripts, agents and other devices through the same door, broker and
//! policy as any app's.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use tilo_actions::{ActionInvoker, InvocationId, InvocationUpdate, ResourceSpec};
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui_core::resources::PublishesResources;
use ui_core::taffy::{NodeId, Style, TaffyTree};
use ui_core::theme::Theme;
use ui_core::vello::kurbo::{Point, Rect};
use ui_core::widget::{
    AppKind, AppModel, AppWidget, Edits, IconDescriptor, ModelPath, TaffyTreeExt as _, Widget,
};

/// One invocation the door admitted, for the host to run.
#[derive(Debug)]
pub struct HostedInvocation {
    pub invocation: InvocationId,
    /// What the action's spec encoded from the caller's arguments.
    pub payload: Vec<u8>,
    /// Who invoked it, as the door admitted them; `None` is the person at
    /// this device acting for themselves.
    pub invoker: Option<ActionInvoker>,
}

#[derive(Default)]
struct Shared {
    resources: RefCell<Vec<ResourceSpec>>,
    /// Bumped whenever `resources` change, so the app publishes them anew.
    generation: Cell<u64>,
    asked: RefCell<VecDeque<HostedInvocation>>,
    answers: RefCell<HashMap<InvocationId, InvocationUpdate>>,
    asked_wake: RefCell<Option<Rc<dyn Fn()>>>,
}

/// The host's side of a [`HostedActions`]: what it publishes, the
/// invocations waiting for it, and its answers.
#[derive(Clone)]
pub struct HostedActionsLink {
    operation: &'static str,
    shared: Rc<Shared>,
}

impl HostedActionsLink {
    /// A link whose actions all encode as operation type `operation`.
    #[must_use]
    pub fn new(operation: &'static str) -> Self {
        Self {
            operation,
            shared: Rc::default(),
        }
    }

    /// Publish `resources` in place of what was published.
    pub fn publish(&self, resources: Vec<ResourceSpec>) {
        *self.shared.resources.borrow_mut() = resources;
        self.shared
            .generation
            .set(self.shared.generation.get().wrapping_add(1));
    }

    /// Call `wake` whenever an invocation arrives, so the host's loop runs.
    pub fn on_asked(&self, wake: impl Fn() + 'static) {
        *self.shared.asked_wake.borrow_mut() = Some(Rc::new(wake));
    }

    /// The invocations admitted since the last call, oldest first.
    #[must_use]
    pub fn take_asked(&self) -> Vec<HostedInvocation> {
        self.shared.asked.borrow_mut().drain(..).collect()
    }

    /// Answer `invocation`: the update its caller observes next. A pending
    /// answer may be followed by a final one.
    pub fn answer(&self, invocation: InvocationId, update: InvocationUpdate) {
        self.shared.answers.borrow_mut().insert(invocation, update);
    }
}

/// The root of a host's app: publishes the link's resources and admits the
/// link's operations ([module docs](self)). It takes no room.
pub struct HostedActions {
    link: HostedActionsLink,
    kind: AppKind,
    title: String,
    icon: IconDescriptor,
    handled: &'static [&'static str],
    published: u64,
    rect: Rect,
}

impl HostedActions {
    /// The root of the app `title`, shown by `icon` as `kind`, answering
    /// through `link`.
    #[must_use]
    pub fn new(
        link: HostedActionsLink,
        kind: AppKind,
        title: String,
        icon: IconDescriptor,
    ) -> Self {
        Self {
            kind,
            title,
            icon,
            // One small allocation per app, which lives as long as it.
            handled: Box::leak(Box::new([link.operation])),
            published: link.shared.generation.get(),
            link,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for HostedActions {
    ui_core::provides!(dyn ResourceOperations, dyn PublishesResources);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                display: ui_core::taffy::Display::None,
                ..Style::default()
            },
            node_map,
            self,
        )
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (0.0, 0.0));
    }
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // What is published changed: the app declares it anew.
        let generation = self.link.shared.generation.get();
        let changed = generation != self.published;
        self.published = generation;
        changed
    }
    fn needs_redraw(&self) -> bool {
        false
    }
    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "HostedActions"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl AppWidget for HostedActions {
    fn app_kind(&self) -> AppKind {
        self.kind
    }
    fn app_title(&self) -> &str {
        &self.title
    }
    fn app_icon(&self) -> IconDescriptor {
        self.icon.clone()
    }
}

impl PublishesResources for HostedActions {
    fn resources(&self, _doc: &AutoCommit) -> Vec<ResourceSpec> {
        self.link.shared.resources.borrow().clone()
    }
}

impl ResourceOperations for HostedActions {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        self.handled
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        _doc: &mut Edits<'_>,
        _path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let (request, _custody) = op.with_custody();
        if request.type_id != self.link.operation {
            return Ok(None);
        }
        op.check()?;
        self.link
            .shared
            .asked
            .borrow_mut()
            .push_back(HostedInvocation {
                invocation: request.invocation,
                payload: request.payload.to_vec(),
                invoker: ui_core::invocation_scope::current_invoker(),
            });
        let wake = self.link.shared.asked_wake.borrow().clone();
        if let Some(wake) = wake {
            wake();
        }
        Ok(Some(ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<InvocationUpdate> {
        let mut answers = self.link.shared.answers.borrow_mut();
        let update = answers.get(&invocation)?.clone();
        if update.is_terminal() {
            answers.remove(&invocation);
        }
        Some(update)
    }
}
