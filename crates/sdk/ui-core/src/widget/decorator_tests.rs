use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::widget::TaffyTreeExt;
use automerge::ReadDoc;

/// The method names declared at the top level of the block that starts with
/// `header` in `source`: lines indented by exactly four spaces that begin
/// with `fn`. Comment lines (including doc examples) are skipped, and the
/// block ends at the first unindented `}`.
fn block_methods(source: &str, header: &str) -> std::collections::BTreeSet<String> {
    let mut lines = source.lines().skip_while(|line| !line.starts_with(header));
    assert!(lines.next().is_some(), "`{header}` not found");
    lines
        .take_while(|line| *line != "}")
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter_map(|line| line.strip_prefix("    fn "))
        .map(|rest| {
            rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// A method added to `Widget` must also be forwarded by `Decorator`, or every
/// decorator would silently answer it with the `Widget` default instead of
/// the inner widget's answer.
#[test]
fn decorator_forwards_every_widget_method() {
    let widget = block_methods(include_str!("mod.rs"), "pub trait Widget {");
    let source = include_str!("decorator.rs");
    let mut decorator = block_methods(source, "pub trait Decorator {");
    for accessor in ["inner", "inner_mut"] {
        assert!(decorator.remove(accessor), "Decorator::{accessor} missing");
    }
    let blanket = block_methods(source, "impl<T: Decorator> Widget for T {");
    assert!(
        widget.len() > 100,
        "parsed only {} Widget methods",
        widget.len()
    );
    let missing: Vec<_> = widget.difference(&decorator).collect();
    assert!(
        missing.is_empty(),
        "add these Widget methods to `trait Decorator` (forwarding to the inner widget) \
         in widget/decorator.rs: {missing:?}"
    );
    let unforwarded: Vec<_> = widget.difference(&blanket).collect();
    assert!(
        unforwarded.is_empty(),
        "add these methods to `impl<T: Decorator> Widget for T` (calling `Decorator::m`) \
         in widget/decorator.rs: {unforwarded:?}"
    );
    let stale: Vec<_> = decorator.symmetric_difference(&blanket).collect();
    assert!(
        stale.is_empty(),
        "Decorator and its blanket impl disagree: {stale:?}"
    );
}

type Log = Rc<RefCell<Vec<&'static str>>>;

/// A leaf that records which of its methods were called.
struct Recorder {
    log: Log,
    dirty: bool,
    rect: Rect,
}

impl Recorder {
    fn new(log: &Log) -> Self {
        Self {
            log: Rc::clone(log),
            dirty: false,
            rect: Rect::new(1.0, 2.0, 3.0, 4.0),
        }
    }

    fn note(&self, call: &'static str) {
        self.log.borrow_mut().push(call);
    }
}

use vello::kurbo::{Point, Rect};

impl Widget for Recorder {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.note("sync");
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.note("build_layout");
        let node = taffy.leaf_node(taffy::Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        self.note("apply_layout");
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.note("draw");
    }

    fn handle_event_ctx(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
        self.note("handle_event_ctx");
    }

    fn name(&self) -> &'static str {
        "Recorder"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.note("resources");
        vec![tilo_actions::Resource::new("recorder", "test", "Recorder")]
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.note("take_control_invocation");
        None
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn start_resource_operation_with_document(
        &mut self,
        _invocation: tilo_actions::InvocationId,
        _kind: tilo_actions::OperationKind,
        _type_id: &str,
        _payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        use automerge::transaction::Transactable;
        self.note("start_with_document");
        doc.put(path, "admitted", true)
            .map_err(|error| error.to_string())?;
        Ok(Some(ResourceOperationStart::Accepted))
    }
}

/// Adds exactly one hook — a name — and nothing else.
struct Named {
    inner: Recorder,
}

impl Decorator for Named {
    type Inner = Recorder;

    fn inner(&self) -> &Recorder {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Recorder {
        &mut self.inner
    }

    fn name(&self) -> &'static str {
        "Named"
    }
}

fn start(widget: &mut dyn Widget, doc: &mut AutoCommit) -> Option<ResourceOperationStart> {
    widget
        .start_resource_operation_with_document(
            tilo_actions::InvocationId::new(),
            tilo_actions::OperationKind::Effect,
            "test.effect",
            b"{}",
            doc,
            &automerge::ROOT,
        )
        .expect("admission succeeds")
}

#[test]
fn a_decorator_is_its_inner_widget_plus_its_overrides() {
    let log = Log::default();
    let mut decorated = Named {
        inner: Recorder::new(&log),
    };
    decorated.inner.dirty = true;
    let widget: &mut dyn Widget = &mut decorated;
    let mut doc = AutoCommit::new();
    let theme = Theme::dark();
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();

    assert_eq!(widget.name(), "Named");
    assert_eq!(widget.get_rect(), Rect::new(1.0, 2.0, 3.0, 4.0));
    widget.sync(&mut doc, &automerge::ROOT);
    let node = widget.build_layout(&mut taffy, &theme, &mut nodes);
    widget.apply_layout(&taffy, node, Point::ORIGIN);
    widget.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &PresenceCtx::empty(),
    );
    let mut handled = false;
    let mut focus = None;
    widget.handle_event(
        &WindowEvent::Focused(true),
        1.0,
        &winit::event::Modifiers::default(),
        &mut doc,
        &automerge::ROOT,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    assert_eq!(widget.resources(&doc).len(), 1);
    assert!(widget.take_control_invocation().is_none());
    assert_eq!(
        start(widget, &mut doc),
        Some(ResourceOperationStart::Accepted)
    );
    assert!(doc.get(automerge::ROOT, "admitted").unwrap().is_some());
    assert!(widget.take_structure_dirty());
    assert!(!widget.take_structure_dirty(), "reading clears it");
    assert!(widget.children().is_empty(), "the inner widget's children");
    assert!(
        widget.concrete_type_name().ends_with("Named"),
        "identity describes the decorator"
    );

    assert_eq!(
        *log.borrow(),
        [
            "sync",
            "build_layout",
            "apply_layout",
            "draw",
            "handle_event_ctx",
            "resources",
            "take_control_invocation",
            "start_with_document",
        ]
    );
    // The inner widget registered its own node.
    assert_eq!(nodes.len(), 1);
}

/// Overriding `handle_event_ctx` alone is enough: the host calls the
/// positional `handle_event`, which reaches the decorator's override.
#[test]
fn a_decorator_intercepts_input_through_either_entry_point() {
    struct Swallow {
        inner: Recorder,
        seen: usize,
    }
    impl Decorator for Swallow {
        type Inner = Recorder;
        fn inner(&self) -> &Recorder {
            &self.inner
        }
        fn inner_mut(&mut self) -> &mut Recorder {
            &mut self.inner
        }
        fn handle_event_ctx(&mut self, _event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            self.seen += 1;
            *ctx.handled = true;
        }
    }
    let log = Log::default();
    let mut swallow = Swallow {
        inner: Recorder::new(&log),
        seen: 0,
    };
    let mut taffy = TaffyTree::new();
    let node = taffy.leaf_node(taffy::Style::default());
    let (mut handled, mut focus) = (false, None);
    Widget::handle_event(
        &mut swallow,
        &WindowEvent::Focused(true),
        1.0,
        &winit::event::Modifiers::default(),
        &mut AutoCommit::new(),
        &automerge::ROOT,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    assert!(handled);
    assert_eq!(swallow.seen, 1);
    assert!(log.borrow().is_empty(), "the inner widget never saw it");
}

/// A container that only reports its children still delivers mutable
/// operation admission and structure changes from them.
#[test]
fn a_container_without_overrides_forwards_its_children() {
    struct Plain {
        children: Vec<Box<dyn Widget>>,
    }
    impl Widget for Plain {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Plain"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.children.iter().map(AsRef::as_ref).collect()
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.children
                .iter_mut()
                .map(|child| child.as_mut() as &mut dyn Widget)
                .collect()
        }
    }
    let log = Log::default();
    let mut first = Recorder::new(&log);
    first.dirty = true;
    let mut second = Recorder::new(&log);
    second.dirty = true;
    let mut plain = Plain {
        children: vec![Box::new(first), Box::new(second)],
    };
    let mut doc = AutoCommit::new();
    assert_eq!(
        start(&mut plain, &mut doc),
        Some(ResourceOperationStart::Accepted)
    );
    assert!(doc.get(automerge::ROOT, "admitted").unwrap().is_some());
    assert!(plain.take_structure_dirty());
    // Every child was drained, not just the first dirty one.
    assert!(!plain.take_structure_dirty());
}
