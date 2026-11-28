//! Fixtures the tests above the model layer share, through the `test-support` feature: an app
//! widget declared in one line, two windows side by side, and a text field that owns its text.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture's tree is the one it built, so a missing node is a broken fixture"
)]

mod owned_text;
pub use owned_text::OwnedText;

pub use crate::test_app_widget;
/// The kind a test app names ([`test_app_widget!`](crate::test_app_widget)), for the macro to name
/// it where it expands.
#[doc(hidden)]
pub use tilo_ui::widget::AppKind;

#[macro_export]
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
            fn app_kind(&self) -> $crate::testing::AppKind {
                $crate::testing::test_app_widget!(@ or $($kind)?, $crate::testing::AppKind::Editor)
            }
            fn app_title(&self) -> &str {
                $title
            }
            fn app_icon(&self) -> $crate::widget::IconDescriptor {
                $crate::testing::test_app_widget!(
                    @ or $($icon)?, $crate::widget::IconDescriptor::Glyph("T")
                )
            }
            fn permission_hint(&self) -> Option<$crate::testing::AppKind> {
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

/// Two editor windows side by side, as the shell's strip lays them out: 400x300 each in
/// a surface that fills its parent.
pub struct TwoWindows {
    tiles: [crate::widget::TileHost; 2],
    rect: ::vello::kurbo::Rect,
}

impl TwoWindows {
    /// Two stub windows labelled `labels`.
    pub fn new(labels: [&str; 2]) -> Self {
        Self {
            tiles: labels.map(|label| {
                crate::widget::TileHost::new(crate::widget::TileContent::Stub {
                    kind: tilo_ui::widget::AppKind::Editor,
                    label: label.into(),
                })
            }),
            rect: ::vello::kurbo::Rect::ZERO,
        }
    }
}

impl tilo_ui_base::widget::Widget for TwoWindows {
    fn build_layout(
        &mut self,
        taffy: &mut ::taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut ::std::collections::HashMap<
            ::taffy::NodeId,
            *mut dyn tilo_ui_base::widget::Widget,
        >,
    ) -> ::taffy::NodeId {
        use tilo_ui_base::widget::TaffyTreeExt as _;
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
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.rect = rect;
        let children = taffy.children(node).unwrap();
        for (tile, child) in self.tiles.iter_mut().zip(children) {
            tile.apply_layout(taffy, child, ::vello::kurbo::Point::new(rect.x0, rect.y0));
        }
    }

    fn children(&self) -> Vec<&dyn tilo_ui_base::widget::Widget> {
        self.tiles
            .iter()
            .map(|tile| tile as &dyn tilo_ui_base::widget::Widget)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn tilo_ui_base::widget::Widget + '_)> {
        self.tiles
            .iter_mut()
            .map(|tile| tile as &mut dyn tilo_ui_base::widget::Widget)
            .collect()
    }

    fn get_rect(&self) -> ::vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TwoWindows"
    }
}
