//! A hosted app's top layer stands over its own window: the tile, not the
//! screen the desktop's shared layout tree is rooted at.

use super::*;
use crate::widget::TaffyTreeExt;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// An app whose page is one open dialog; it reports where the dialog
/// stood after each layout and top-layer paint.
struct DialogApp {
    dialog: crate::widget::ModalDialog<crate::widget::Text>,
    seen: std::rc::Rc<std::cell::Cell<Rect>>,
}

impl Widget for DialogApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.dialog.build_layout(taffy, theme, node_map);
        taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[child],
        )
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        let child = taffy.children(node).unwrap()[0];
        self.dialog
            .apply_layout(taffy, child, Point::new(rect.x0, rect.y0));
        self.seen.set(self.dialog.dialog_rect());
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.dialog]
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.dialog
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.seen.set(self.dialog.dialog_rect());
    }
    fn name(&self) -> &'static str {
        "DialogApp"
    }
}

impl crate::widget::app_widget::AppWidget for DialogApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Dialog"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("D")
    }
}

/// The desktop lays every window out in one tree rooted at the screen.
/// An app's dialog must centre on, and stay inside, its own window, not
/// the screen: in a window narrower than the screen a screen-centred
/// sheet spilled past the window's edge.
#[test]
fn an_apps_dialog_centres_inside_its_window_not_the_screen() {
    use taffy::prelude::TaffyMaxContent;
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let seen = std::rc::Rc::new(std::cell::Cell::new(Rect::ZERO));
    let dialog = crate::widget::ModalDialog::new(
        "Back up vault",
        crate::widget::Text::new("A backup keeps every item."),
    )
    .open(true);
    let mut host = TileHost::new(TileContent::App(Box::new(DialogApp {
        dialog,
        seen: seen.clone(),
    })));
    // The screen, with the window standing in its left two-thirds below
    // a top bar, as a window does in the strip.
    let screen = Rect::new(0.0, 0.0, 1440.0, 900.0);
    let mut tree = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    let tile = host.build_layout(&mut tree, &theme, &mut nodes);
    let mut style = tree.style(tile).unwrap().clone();
    style.size = Size {
        width: Dimension::length(960.0),
        height: Dimension::length(860.0),
    };
    tree.set_style(tile, style).unwrap();
    let root = tree.parent_node(
        Style {
            size: Size {
                width: Dimension::length(1440.0),
                height: Dimension::length(900.0),
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(0.0),
                right: taffy::LengthPercentage::length(0.0),
                top: taffy::LengthPercentage::length(40.0),
                bottom: taffy::LengthPercentage::length(0.0),
            },
            ..Default::default()
        },
        &[tile],
    );
    tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
    host.apply_layout(&tree, tile, Point::ORIGIN);
    let window = host.get_rect();
    assert_eq!(window, Rect::new(0.0, 40.0, 960.0, 900.0));
    let inside = |sheet: Rect, when: &str| {
        assert!(sheet.width() > 0.0, "{when}: the dialog is laid out");
        assert!(
            (sheet.center().x - window.center().x).abs() < 1.0,
            "{when}: the sheet {sheet:?} is centred on its window {window:?}"
        );
        assert!(
            sheet.x0 >= window.x0
                && sheet.x1 <= window.x1
                && sheet.y0 >= window.y0
                && sheet.y1 <= window.y1,
            "{when}: the sheet {sheet:?} stays inside its window {window:?}"
        );
    };
    inside(seen.get(), "before its first paint");

    // The top layer is drawn over the whole screen.
    let presence = PresenceCtx::default();
    presence.viewport.set(Some(screen));
    host.draw_overlay(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &presence,
    );
    inside(seen.get(), "painted");
    assert_eq!(
        presence.viewport.get(),
        Some(screen),
        "the screen's viewport is restored for the next window"
    );
}
