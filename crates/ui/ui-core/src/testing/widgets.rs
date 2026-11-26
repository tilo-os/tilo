//! Boilerplate for the probe widgets this crate's own tests define. Each macro expands
//! inside an `impl Widget for ...` (or produces the whole `impl AppWidget`) so a probe
//! only spells out the methods it observes.

/// Stubs for [`Widget`](crate::widget::Widget) methods a probe does not observe; it emits
/// exactly the named pieces.
macro_rules! inert_widget_methods {
    (@ leaf_layout) => {
        fn build_layout(
            &mut self,
            tree: &mut ::taffy::TaffyTree<()>,
            _: &$crate::theme::Theme,
            nodes: &mut ::std::collections::HashMap<
                ::taffy::NodeId,
                *mut dyn $crate::widget::Widget,
            >,
        ) -> ::taffy::NodeId {
            let node = tree.new_leaf(::taffy::Style::default()).unwrap();
            nodes.insert(node, ::std::ptr::from_mut::<dyn $crate::widget::Widget>(self));
            node
        }
    };
    (@ unregistered_leaf_layout) => {
        fn build_layout(
            &mut self,
            tree: &mut ::taffy::TaffyTree<()>,
            _: &$crate::theme::Theme,
            _: &mut ::std::collections::HashMap<::taffy::NodeId, *mut dyn $crate::widget::Widget>,
        ) -> ::taffy::NodeId {
            tree.new_leaf(::taffy::Style::default()).unwrap()
        }
    };
    (@ name = $name:literal) => {
        fn name(&self) -> &str {
            $name
        }
    };
    (@ name_field) => {
        fn name(&self) -> &str {
            self.name
        }
    };
    ($($method:ident $(= $value:literal)?),+ $(,)?) => {
        $($crate::testing::inert_widget_methods!(@ $method $(= $value)?);)+
    };
}

macro_rules! delegate_widget_methods {
    (@ $field:tt sync) => {
        fn sync(&mut self, doc: &$crate::widget::AppModel, path: &$crate::widget::ModelPath) {
            self.$field.sync(doc, path);
        }
    };
    (@ $field:tt build_layout) => {
        fn build_layout(
            &mut self,
            tree: &mut ::taffy::TaffyTree<()>,
            theme: &$crate::theme::Theme,
            nodes: &mut ::std::collections::HashMap<
                ::taffy::NodeId,
                *mut dyn $crate::widget::Widget,
            >,
        ) -> ::taffy::NodeId {
            self.$field.build_layout(tree, theme, nodes)
        }
    };
    (@ $field:tt apply_layout) => {
        fn apply_layout(
            &mut self,
            tree: &::taffy::TaffyTree<()>,
            node: ::taffy::NodeId,
            origin: ::vello::kurbo::Point,
        ) {
            self.$field.apply_layout(tree, node, origin);
        }
    };
    (@ $field:tt draw) => {
        fn draw(&mut self, ctx: &mut $crate::draw::DrawCtx<'_>) {
            self.$field.draw(ctx);
        }
    };
    (@ $field:tt handle_event) => {
        fn handle_event(
            &mut self,
            event: &::winit::event::WindowEvent,
            ctx: &mut $crate::widget::EventCtx<'_>,
        ) {
            self.$field.handle_event(event, ctx);
        }
    };
    (@ $field:tt get_rect) => {
        fn get_rect(&self) -> ::vello::kurbo::Rect {
            self.$field.get_rect()
        }
    };
    ($field:tt: $($method:ident),+ $(,)?) => {
        $($crate::testing::delegate_widget_methods!(@ $field $method);)+
    };
}

macro_rules! test_app_widget {
    (@ or , $default:expr) => {
        $default
    };
    (@ or $value:expr, $default:expr) => {
        $value
    };
    (
        @impl [$($generics:tt)*] $ty:ty, $title:literal
        $(, icon = $icon:expr)?
        $(, kind = $kind:expr)?
        $(, { $($extra:tt)* })?
    ) => {
        impl<$($generics)*> $crate::widget::AppWidget for $ty {
            fn app_kind(&self) -> $crate::widget::AppKind {
                $crate::testing::test_app_widget!(@ or $($kind)?, $crate::widget::AppKind::Editor)
            }
            fn app_title(&self) -> &str {
                $title
            }
            fn app_icon(&self) -> $crate::widget::IconDescriptor {
                $crate::testing::test_app_widget!(
                    @ or $($icon)?, $crate::widget::IconDescriptor::Glyph("T")
                )
            }
            fn permission_hint(&self) -> Option<$crate::widget::AppKind> {
                None
            }
            $($($extra)*)?
        }
    };
    (impl [$($generics:tt)*] $ty:ty, $($rest:tt)*) => {
        $crate::testing::test_app_widget!(@impl [$($generics)*] $ty, $($rest)*);
    };
    ($ty:ty, $($rest:tt)*) => {
        $crate::testing::test_app_widget!(@impl [] $ty, $($rest)*);
    };
}

pub(crate) use {delegate_widget_methods, inert_widget_methods, test_app_widget};

#[cfg(feature = "model")]
/// Two editor windows side by side, as the shell's strip lays them out: 400x300 each in
/// a surface that fills its parent.
pub(crate) struct TwoWindows {
    tiles: [crate::widget::TileHost; 2],
    rect: ::vello::kurbo::Rect,
}

#[cfg(feature = "model")]
impl TwoWindows {
    /// Two stub windows labelled `labels`.
    pub(crate) fn new(labels: [&str; 2]) -> Self {
        Self {
            tiles: labels.map(|label| {
                crate::widget::TileHost::new(crate::widget::TileContent::Stub {
                    kind: crate::widget::AppKind::Editor,
                    label: label.into(),
                })
            }),
            rect: ::vello::kurbo::Rect::ZERO,
        }
    }
}

#[cfg(feature = "model")]
impl crate::widget::Widget for TwoWindows {
    fn build_layout(
        &mut self,
        taffy: &mut ::taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut ::std::collections::HashMap<::taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> ::taffy::NodeId {
        use crate::widget::TaffyTreeExt as _;
        let mut nodes = Vec::new();
        for tile in &mut self.tiles {
            let node = tile.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(node).unwrap().clone();
            style.size = ::taffy::Size {
                width: ::taffy::Dimension::length(400.0),
                height: ::taffy::Dimension::length(300.0),
            };
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            taffy.set_style(node, style).unwrap();
            nodes.push(node);
        }
        taffy.parent_node(
            ::taffy::Style {
                size: ::taffy::Size {
                    width: ::taffy::Dimension::percent(1.0),
                    height: ::taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &nodes,
        )
    }

    fn apply_layout(
        &mut self,
        taffy: &::taffy::TaffyTree<()>,
        node: ::taffy::NodeId,
        origin: ::vello::kurbo::Point,
    ) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        self.rect = rect;
        let children = taffy.children(node).unwrap();
        for (tile, child) in self.tiles.iter_mut().zip(children) {
            tile.apply_layout(taffy, child, ::vello::kurbo::Point::new(rect.x0, rect.y0));
        }
    }

    fn children(&self) -> Vec<&dyn crate::widget::Widget> {
        self.tiles
            .iter()
            .map(|tile| tile as &dyn crate::widget::Widget)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        self.tiles
            .iter_mut()
            .map(|tile| tile as &mut dyn crate::widget::Widget)
            .collect()
    }

    fn get_rect(&self) -> ::vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TwoWindows"
    }
}

/// How much room a [`TestLeaf`] asks for.
#[derive(Clone, Copy)]
enum LeafSize {
    /// All the space its parent gives it.
    Fill,
    /// A fixed width and height.
    Fixed(f32, f32),
    /// As small as its (absent) content.
    Auto,
}

/// What a [`TestLeaf`] was shown and sent, shared with the test that placed it.
#[derive(Clone, Default)]
pub(crate) struct Seen(::std::rc::Rc<::std::cell::RefCell<SeenLog>>);

#[derive(Default)]
struct SeenLog {
    events: usize,
    pointer: usize,
    presses: usize,
    focused: bool,
}

impl Seen {
    /// Every event the leaf was sent.
    pub(crate) fn events(&self) -> usize {
        self.0.borrow().events
    }

    /// Pointer moves and button changes the leaf was sent.
    pub(crate) fn pointer(&self) -> usize {
        self.0.borrow().pointer
    }

    /// Left-button presses the leaf was sent.
    pub(crate) fn presses(&self) -> usize {
        self.0.borrow().presses
    }

    /// Whether the leaf holds keyboard focus.
    pub(crate) fn focused(&self) -> bool {
        self.0.borrow().focused
    }
}

/// The one leaf this crate's tests put inside containers, scrolls, dialogs and shells to
/// see where they placed it and what reached it: it fills its parent, or has a fixed size,
/// answers to a test id, can take focus on a press and swallow the events it is sent, and
/// records what reached it in a [`Seen`] the test keeps. Containers in tests are the real
/// ones (`Column`, `Row`, `Scrollable`), holding these leaves.
pub(crate) struct TestLeaf {
    pub(crate) id: &'static str,
    pub(crate) rect: ::vello::kurbo::Rect,
    size: LeafSize,
    focusable: bool,
    pub(crate) consumes: bool,
    seen: Seen,
}

impl TestLeaf {
    fn with_size(id: &'static str, size: LeafSize) -> Self {
        Self {
            id,
            rect: ::vello::kurbo::Rect::ZERO,
            size,
            focusable: false,
            consumes: false,
            seen: Seen::default(),
        }
    }

    /// A leaf that fills the space it is given, answering to `id`.
    pub(crate) fn fill(id: &'static str) -> Self {
        Self::with_size(id, LeafSize::Fill)
    }

    /// A leaf `width` by `height`.
    pub(crate) fn sized(width: f32, height: f32) -> Self {
        Self::with_size("leaf", LeafSize::Fixed(width, height))
    }

    /// A leaf with no size of its own.
    pub(crate) fn auto() -> Self {
        Self::with_size("leaf", LeafSize::Auto)
    }

    /// The leaf takes keyboard focus, and a left press focuses it.
    pub(crate) fn focusable(mut self) -> Self {
        self.focusable = true;
        self
    }

    /// The leaf marks every event it is sent handled, so nothing behind it sees it.
    pub(crate) fn consumes(mut self) -> Self {
        self.consumes = true;
        self
    }

    /// What the leaf will be shown and sent.
    pub(crate) fn seen(&self) -> Seen {
        self.seen.clone()
    }

    /// The leaf as an element, with what it will be shown and sent.
    pub(crate) fn boxed(self) -> (crate::widget::Element, Seen) {
        let seen = self.seen();
        (Box::new(self), seen)
    }
}

impl crate::widget::Widget for TestLeaf {
    fn build_layout(
        &mut self,
        taffy: &mut ::taffy::TaffyTree<()>,
        _: &crate::theme::Theme,
        node_map: &mut ::std::collections::HashMap<::taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> ::taffy::NodeId {
        let size = match self.size {
            LeafSize::Fill => ::taffy::Size::from_percent(1.0, 1.0),
            LeafSize::Fixed(width, height) => ::taffy::Size::from_lengths(width, height),
            LeafSize::Auto => ::taffy::Size::auto(),
        };
        let style = ::taffy::Style {
            size,
            ..::taffy::Style::default()
        };
        crate::widget::TaffyTreeExt::widget_leaf(taffy, style, node_map, self)
    }

    crate::rect_geometry!();

    fn handle_event(
        &mut self,
        event: &::winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        use ::winit::event::{ElementState, MouseButton, WindowEvent};
        let mut seen = self.seen.0.borrow_mut();
        seen.events = seen.events.saturating_add(1);
        match event {
            WindowEvent::CursorMoved { .. } => seen.pointer = seen.pointer.saturating_add(1),
            WindowEvent::MouseInput { state, button, .. } => {
                seen.pointer = seen.pointer.saturating_add(1);
                if *state == ElementState::Pressed && *button == MouseButton::Left {
                    seen.presses = seen.presses.saturating_add(1);
                    if self.focusable {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
            }
            _ => {}
        }
        if self.consumes {
            *ctx.handled = true;
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == ::vello::kurbo::Rect::ZERO
    }

    fn is_focusable(&self) -> bool {
        self.focusable
    }

    fn set_focus(&mut self, focused: bool) {
        self.seen.0.borrow_mut().focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.seen.focused()
    }

    fn reveal_descendant(
        &mut self,
        target: *const dyn crate::widget::Widget,
        _: ::vello::kurbo::Rect,
    ) -> bool {
        ::std::ptr::addr_eq(
            ::std::ptr::from_ref::<dyn crate::widget::Widget>(self),
            target,
        )
    }

    fn name(&self) -> &'static str {
        "TestLeaf"
    }

    fn as_any(&self) -> Option<&dyn ::std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn ::std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }
}
