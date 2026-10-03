//! Boilerplate for the probe widgets this crate's own tests define.
//!
//! Each macro expands inside an `impl Widget for ...` (or produces the whole
//! `impl AppWidget`) so a probe only spells out the methods it observes.

/// No-op implementations of required [`Widget`](crate::widget::Widget)
/// methods. Without arguments it emits `apply_layout`, `draw` and a zero
/// `get_rect`; with a list it emits exactly the named pieces:
///
/// - `apply_layout`, `draw`: do nothing
/// - `get_rect`: [`Rect::ZERO`](vello::kurbo::Rect::ZERO)
/// - `leaf_layout`: a default-styled leaf registered for this widget
/// - `unregistered_leaf_layout`: a default-styled leaf left out of the node map
/// - `name = "..."`: a fixed widget name
/// - `name_field`: the name stored in `self.name`
///
/// `handle_event` needs no stub: the trait default ignores every event.
macro_rules! inert_widget_methods {
    () => {
        $crate::testing::inert_widget_methods!(apply_layout, draw, get_rect);
    };
    (@ apply_layout) => {
        fn apply_layout(
            &mut self,
            _: &::taffy::TaffyTree<()>,
            _: ::taffy::NodeId,
            _: ::vello::kurbo::Point,
        ) {
        }
    };
    (@ draw) => {
        fn draw(
            &mut self,
            _: &mut ::vello::Scene,
            _: ::vello::kurbo::Affine,
            _: &mut $crate::text_render::TextRenderer,
            _: &$crate::theme::Theme,
            _: &$crate::presence::PresenceCtx,
        ) {
        }
    };
    (@ get_rect) => {
        fn get_rect(&self) -> ::vello::kurbo::Rect {
            ::vello::kurbo::Rect::ZERO
        }
    };
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

/// Forwards the named required [`Widget`](crate::widget::Widget) methods to
/// the widget in field `$field` (a name or a tuple index):
/// `delegate_widget_methods!(inner: sync, build_layout, apply_layout, draw,
/// handle_event, get_rect)`.
macro_rules! delegate_widget_methods {
    (@ $field:tt sync) => {
        fn sync(&mut self, doc: &mut $crate::widget::AppModel, path: &$crate::widget::ModelPath) {
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

/// An [`AppWidget`](crate::widget::AppWidget) for a probe: an editor with the
/// given title, a `T` glyph and no permission hint. `icon = ...` and
/// `kind = ...` override the defaults, a leading `impl [generics]` adds impl
/// parameters, and a trailing `{ ... }` block adds further methods.
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
