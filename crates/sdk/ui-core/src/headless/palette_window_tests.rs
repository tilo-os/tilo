//! The host's palette stands over the window that asked for it, as an app's
//! own palette stands over its window: in the browser desktop a
//! confirmation one window asked for was centred on the whole screen,
//! across the window beside it.

use super::FrameHarness;
use crate::widget::{AppKind, TaffyTreeExt as _, TileContent, TileHost, Widget};
use std::collections::HashMap;
use vello::kurbo::{Point, Rect};

/// Two windows side by side, as the desktop's strip lays them out.
struct TwoWindows {
    tiles: [TileHost; 2],
    rect: Rect,
}

impl Widget for TwoWindows {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let mut nodes = Vec::new();
        for tile in &mut self.tiles {
            let node = tile.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(node).unwrap().clone();
            style.size = taffy::Size {
                width: taffy::Dimension::length(400.0),
                height: taffy::Dimension::length(300.0),
            };
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            taffy.set_style(node, style).unwrap();
            nodes.push(node);
        }
        taffy.parent_node(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &nodes,
        )
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        self.rect = rect;
        let children = taffy.children(node).unwrap();
        for (tile, child) in self.tiles.iter_mut().zip(children) {
            tile.apply_layout(taffy, child, Point::new(rect.x0, rect.y0));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.tiles.iter().map(|tile| tile as &dyn Widget).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.tiles
            .iter_mut()
            .map(|tile| tile as &mut dyn Widget)
            .collect()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TwoWindows"
    }
}

fn stub(label: &str) -> TileHost {
    TileHost::new(TileContent::Stub {
        kind: AppKind::Editor,
        label: label.into(),
    })
}

/// A confirmation a press asked for stands over the window pressed in, not
/// over the whole surface; after a key it no longer follows that press.
#[test]
fn a_confirmation_a_press_asked_for_stands_over_the_window_pressed_in() {
    let _settled = crate::motion::testing::settled();
    let mut harness = FrameHarness::new(
        crate::State::new(),
        Box::new(TwoWindows {
            tiles: [stub("Chat"), stub("Slides")],
            rect: Rect::ZERO,
        }),
        (800, 300),
    );
    harness.relayout();
    assert_eq!(
        harness.invoking_window_rect(),
        None,
        "with no press and no focus in a window, the palette has the surface"
    );
    harness.click_at(600.0, 150.0);
    harness.relayout();
    assert_eq!(
        harness.invoking_window_rect(),
        Some(Rect::new(400.0, 0.0, 800.0, 300.0)),
        "the palette stands over the window pressed in"
    );
    harness.send(&crate::synthetic_input::named_key(
        winit::keyboard::NamedKey::Shift,
    ));
    assert_eq!(
        harness.pressed_at, None,
        "after a key the press no longer decides; the focused window does"
    );
}
