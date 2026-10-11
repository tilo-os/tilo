//! Boilerplate for the probe widgets this crate's own tests define.
//!
//! Each macro expands inside an `impl Widget for ...` (or produces the whole
//! `impl AppWidget`) so a probe only spells out the methods it observes.

/// Forwards the named required [`Widget`](crate::widget::Widget) methods to
/// the widget in field `$field` (a name or a tuple index):
/// `delegate_widget_methods!(inner: sync, build_layout, apply_layout, draw,
/// handle_event, get_rect)`.
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

pub(crate) use {delegate_widget_methods, test_app_widget};
