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
        fn draw(
            &mut self,
            scene: &mut ::vello::Scene,
            transform: ::vello::kurbo::Affine,
            text: &mut $crate::text_render::TextRenderer,
            theme: &$crate::theme::Theme,
            presence: &$crate::presence::PresenceCtx,
        ) {
            self.$field.draw(scene, transform, text, theme, presence);
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

/// Two editor windows side by side, as the desktop's strip lays them out: 400x300 each in
/// a surface that fills its parent.
pub(crate) struct TwoWindows {
    tiles: [crate::widget::TileHost; 2],
    rect: ::vello::kurbo::Rect,
}

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
