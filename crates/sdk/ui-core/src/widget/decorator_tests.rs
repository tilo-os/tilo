use crate::widget::{AppModel, ModelPath};
use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::testing::{Dispatch, dispatch};
use crate::widget::TaffyTreeExt;
use crate::widget::{ResourceOperation, ResourceOperationStart};
use automerge::ReadDoc;
use automerge::{AutoCommit, ObjId};

/// The method names declared at the top level of the block that opens with
/// the line `header` in `source`: lines indented four spaces deeper than the
/// header that begin with `fn`. Comment lines (including doc examples) are
/// skipped, and the block ends at the header's closing `}`.
fn block_methods(source: &str, header: &str) -> std::collections::BTreeSet<String> {
    let indent = &header[..header.len() - header.trim_start().len()];
    let close = format!("{indent}}}");
    let method = format!("{indent}    fn ");
    let mut lines = source.lines().skip_while(|line| *line != header);
    assert!(lines.next().is_some(), "`{header}` not found");
    lines
        .take_while(|line| *line != close)
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter_map(|line| line.strip_prefix(method.as_str()))
        .map(|rest| {
            rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[test]
fn decorators_forward_override_and_intercept_their_inner_widget() {
    /// A method added to `Widget` must also be forwarded by `Decorator`, or every
    /// decorator would silently answer it with the `Widget` default instead of
    /// the inner widget's answer.
    fn decorator_forwards_every_widget_method() {
        let widget = block_methods(include_str!("mod.rs"), "pub trait Widget {");
        let source = include_str!("decorator.rs");
        // `decorator_trait!` turns each `ref`/`mut` line into both copies;
        // the methods written out by hand must appear in both blocks.
        let mut own = block_methods(source, "    trait {");
        for accessor in ["inner", "inner_mut"] {
            assert!(own.remove(accessor), "Decorator::{accessor} missing");
        }
        let blanket_own = block_methods(source, "    impl {");
        let mut forwarded = block_methods(source, "    ref {");
        forwarded.extend(block_methods(source, "    mut {"));
        assert!(
            widget.len() > 20,
            "parsed only {} Widget methods",
            widget.len()
        );
        let mut decorator = own.clone();
        decorator.extend(forwarded.iter().cloned());
        let missing: Vec<_> = widget.difference(&decorator).collect();
        assert!(
            missing.is_empty(),
            "add these Widget methods to the `ref` or `mut` list of `decorator_trait!` \
         in widget/decorator.rs: {missing:?}"
        );
        let stale: Vec<_> = own.symmetric_difference(&blanket_own).collect();
        assert!(
            stale.is_empty(),
            "write these methods under both `trait` and `impl` of `decorator_trait!`: {stale:?}"
        );
    }

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
        widget.sync(&doc, &automerge::ROOT);
        let node = widget.build_layout(&mut taffy, &theme, &mut nodes);
        widget.apply_layout(&taffy, node, Point::ORIGIN);
        widget.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
        );
        Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node)
            .send(widget, &WindowEvent::Focused(true));
        assert_eq!(crate::resources::resources(widget, &doc).len(), 1);
        assert!(crate::widget::controls::take_control_invocation(widget).is_none());
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
                "handle_event",
                "resources",
                "take_control_invocation",
                "start_operation",
            ]
        );
        // The inner widget registered its node; the decorator stands in for
        // it, so a host reaching the node meets the decorator.
        assert_eq!(nodes.len(), 1);
        let registered = nodes[&node];
        // SAFETY: `decorated` is alive and was laid out into `nodes` above.
        let registered = unsafe { &*registered };
        assert!(registered.concrete_type_name().ends_with("Named"));
        assert_eq!(registered.name(), "Named");
    }

    /// A decorator's `handle_event` override sees input before its inner widget.
    fn a_decorator_intercepts_input() {
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
            fn handle_event(&mut self, _event: &WindowEvent, ctx: &mut EventCtx<'_>) {
                self.seen += 1;
                *ctx.handled = true;
            }
        }
        let log = Log::default();
        let mut swallow = Swallow {
            inner: Recorder::new(&log),
            seen: 0,
        };
        assert!(dispatch(&mut swallow, &WindowEvent::Focused(true)));
        assert_eq!(swallow.seen, 1);
        assert!(log.borrow().is_empty(), "the inner widget never saw it");
    }

    /// A container that only reports its children still delivers mutable
    /// operation admission and structure changes from them.
    fn a_container_without_overrides_forwards_its_children() {
        struct Plain {
            children: Vec<Box<dyn Widget>>,
        }
        impl Widget for Plain {
            fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
            }
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _theme: &Theme,
                _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                taffy.leaf_node(taffy::Style::default())
            }
            fn name(&self) -> &'static str {
                "Plain"
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

    decorator_forwards_every_widget_method();
    a_decorator_is_its_inner_widget_plus_its_overrides();
    a_decorator_intercepts_input();
    a_container_without_overrides_forwards_its_children();
}

type Log = Rc<RefCell<Vec<&'static str>>>;

/// A leaf that records which of its methods were called.
struct Recorder {
    log: Log,
    dirty: bool,
    rect: Rect,
    physical: Option<super::super::PhysicalInput>,
}

impl Recorder {
    fn new(log: &Log) -> Self {
        Self {
            log: Rc::clone(log),
            dirty: false,
            rect: Rect::new(1.0, 2.0, 3.0, 4.0),
            physical: None,
        }
    }

    fn note(&self, call: &'static str) {
        self.log.borrow_mut().push(call);
    }
}

use vello::kurbo::{Point, Rect};

impl Widget for Recorder {
    fn observe_physical_input(&mut self, input: super::super::PhysicalInput) {
        self.physical = Some(input);
    }

    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.note("sync");
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.note("build_layout");
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
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

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
        self.note("handle_event");
    }

    fn name(&self) -> &'static str {
        "Recorder"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

impl crate::widget::ResourceOperations for Recorder {
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        use automerge::transaction::Transactable;
        if op.unscoped().is_none() {
            return Ok(None);
        }
        self.note("start_operation");
        doc.document_mut()
            .put(path, "admitted", true)
            .map_err(|error| error.to_string())?;
        Ok(Some(ResourceOperationStart::Accepted))
    }
}

impl crate::resources::PublishesResources for Recorder {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.note("resources");
        vec![tilo_actions::Resource::new("recorder", "test", "Recorder")]
    }
}

impl crate::widget::controls::ActionControls for Recorder {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.note("take_control_invocation");
        None
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
    ResourceOperation::local(
        tilo_actions::InvocationId::new(),
        tilo_actions::OperationKind::Effect,
        "test.effect",
        b"{}",
    )
    .start(
        widget,
        &mut crate::widget::Edits::lend(doc),
        &automerge::ROOT,
    )
    .expect("admission succeeds")
}

#[test]
fn decorators_preserve_content_free_physical_feedback() {
    use crate::widget::PhysicalInput;
    let mut decorated = Named {
        inner: Recorder::new(&Log::default()),
    };
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    Widget::observe_physical_input(
        &mut decorated,
        PhysicalInput::Key {
            key,
            state: winit::event::ElementState::Pressed,
        },
    );
    assert!(matches!(decorated.inner.physical, Some(PhysicalInput::Key {
        key: observed, state: winit::event::ElementState::Pressed,
    }) if observed == key));
    Widget::observe_physical_input(&mut decorated, PhysicalInput::Focused(false));
    assert!(matches!(
        decorated.inner.physical,
        Some(PhysicalInput::Focused(false))
    ));
}

/// [`Named`] through [`concrete::Decorator`] and a concrete [`Widget`] impl,
/// as the toolkit's own boxed decorators are.
struct ConcreteNamed {
    inner: Recorder,
}

impl concrete::Decorator for ConcreteNamed {
    type Inner = Recorder;

    fn inner(&self) -> &Recorder {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Recorder {
        &mut self.inner
    }

    fn name(&self) -> &'static str {
        "ConcreteNamed"
    }
}

concrete_decorator_widget!(ConcreteNamed);

/// A toolkit decorator with a concrete `Widget` impl (compiled once into the
/// toolkit rather than in every app that boxes it) is the same widget as one
/// through the blanket impl: its inner widget plus its overrides, standing in
/// for the inner widget's layout node.
#[test]
fn a_concrete_decorator_forwards_and_stands_in_like_the_blanket_one() {
    let log = Log::default();
    let mut decorated = ConcreteNamed {
        inner: Recorder::new(&log),
    };
    decorated.inner.dirty = true;
    let widget: &mut dyn Widget = &mut decorated;
    let doc = AutoCommit::new();
    let theme = Theme::dark();
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();

    assert_eq!(widget.name(), "ConcreteNamed");
    assert_eq!(widget.get_rect(), Rect::new(1.0, 2.0, 3.0, 4.0));
    widget.sync(&doc, &automerge::ROOT);
    let node = widget.build_layout(&mut taffy, &theme, &mut nodes);
    widget.apply_layout(&taffy, node, Point::ORIGIN);
    widget.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &PresenceCtx::empty(),
    );
    assert!(widget.take_structure_dirty());
    assert!(!widget.take_structure_dirty(), "reading clears it");
    assert!(widget.children().is_empty(), "the inner widget's children");
    assert!(
        widget.concrete_type_name().ends_with("ConcreteNamed"),
        "identity describes the decorator"
    );
    assert!(
        widget.as_any().is_some_and(|any| any.is::<ConcreteNamed>()),
        "downcasts to the decorator"
    );
    assert_eq!(
        *log.borrow(),
        ["sync", "build_layout", "apply_layout", "draw"]
    );
    assert_eq!(nodes.len(), 1);
    // SAFETY: `decorated` is alive and was laid out into `nodes` above.
    let registered = unsafe { &*nodes[&node] };
    assert!(registered.concrete_type_name().ends_with("ConcreteNamed"));
    assert_eq!(registered.name(), "ConcreteNamed");
}
