//! No-op stubs for the required [`Widget`](crate::widget::Widget) methods
//! probe widgets don't observe, shared with downstream tests through the
//! `test-support` feature.

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
#[doc(hidden)]
#[macro_export]
macro_rules! __inert_widget_methods {
    () => {
        $crate::testing::inert_widget_methods!(apply_layout, draw, get_rect);
    };
    (@ apply_layout) => {
        fn apply_layout(
            &mut self,
            _: &$crate::taffy::TaffyTree<()>,
            _: $crate::taffy::NodeId,
            _: $crate::vello::kurbo::Point,
        ) {
        }
    };
    (@ draw) => {
        fn draw(
            &mut self,
            _: &mut $crate::vello::Scene,
            _: $crate::vello::kurbo::Affine,
            _: &mut $crate::text_render::TextRenderer,
            _: &$crate::theme::Theme,
            _: &$crate::presence::PresenceCtx,
        ) {
        }
    };
    (@ get_rect) => {
        fn get_rect(&self) -> $crate::vello::kurbo::Rect {
            $crate::vello::kurbo::Rect::ZERO
        }
    };
    (@ leaf_layout) => {
        fn build_layout(
            &mut self,
            tree: &mut $crate::taffy::TaffyTree<()>,
            _: &$crate::theme::Theme,
            nodes: &mut ::std::collections::HashMap<
                $crate::taffy::NodeId,
                *mut dyn $crate::widget::Widget,
            >,
        ) -> $crate::taffy::NodeId {
            let node = tree.new_leaf($crate::taffy::Style::default()).unwrap();
            nodes.insert(node, ::std::ptr::from_mut::<dyn $crate::widget::Widget>(self));
            node
        }
    };
    (@ unregistered_leaf_layout) => {
        fn build_layout(
            &mut self,
            tree: &mut $crate::taffy::TaffyTree<()>,
            _: &$crate::theme::Theme,
            _: &mut ::std::collections::HashMap<$crate::taffy::NodeId, *mut dyn $crate::widget::Widget>,
        ) -> $crate::taffy::NodeId {
            tree.new_leaf($crate::taffy::Style::default()).unwrap()
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

pub use __inert_widget_methods as inert_widget_methods;
