//! The bar's room in the desktop's column, for a desktop that draws its bar elsewhere (the shell
//! hosts the bar as an app over the desktop).

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::topbar_height;
use crate::chrome::TopBarPresence;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// How the bar stands on its display, which its room in the column follows.
pub type Standing = Rc<dyn Fn() -> TopBarPresence>;

/// Takes the workspace [`super::TopBarChrome`] takes in the desktop's column
/// (none under a fullscreen window, none while the person hides the bar,
/// else the bar's height) and draws nothing.
pub struct BarSpace {
    standing: Standing,
    laid_out: Option<TopBarPresence>,
    rect: Rect,
}

impl BarSpace {
    #[must_use]
    pub fn new(standing: Standing) -> Self {
        Self {
            standing,
            laid_out: None,
            rect: Rect::ZERO,
        }
    }

    fn now(&self) -> TopBarPresence {
        (self.standing)()
    }

    fn stale(&self) -> bool {
        self.laid_out != Some(self.now())
    }
}

/// The strip this workspace takes while the bar is docked, and none while the bar is hidden or
/// shown over the window covering its display: either way the window takes the whole display.
fn space_style(presence: TopBarPresence, theme: &Theme) -> Style {
    Style {
        display: if presence == TopBarPresence::Docked {
            Display::Flex
        } else {
            Display::None
        },
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::length(topbar_height(theme)),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

impl Widget for BarSpace {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn name(&self) -> &'static str {
        "TopBarSpace"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let now = self.now();
        self.laid_out = Some(now);
        let style = space_style(now, theme);
        let node = match taffy.new_leaf(style) {
            Ok(node) => node,
            Err(err) => {
                tracing::error!("topbar space: leaf allocation failed: {err}");
                NodeId::from(0u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = taffy.layout(node) {
            self.rect = ui::widget::placed(layout, origin);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Room and nothing in it: an empty element where the bar's room stands.
    fn html(&self, _cx: &mut ui::composition::HtmlCx<'_>) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn is_layout_dirty(&self) -> bool {
        self.stale()
    }

    /// It draws nothing: what it takes is room in the column, which a settled tree keeps.
    fn needs_redraw(&self) -> bool {
        false
    }

    /// Docking or hiding the bar moves the windows, so the tree it was laid out in is rebuilt.
    fn take_structure_dirty(&mut self) -> bool {
        self.stale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn the_room_follows_the_bar_showing_over_a_window() {
        let standing = Rc::new(Cell::new(TopBarPresence::Docked));
        let read = standing.clone();
        let mut space = BarSpace::new(Rc::new(move || read.get()));
        let theme = Theme::default();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        let style = taffy.style(node).unwrap().clone();
        assert_eq!(style.display, Display::Flex);
        assert_eq!(
            style.size.height,
            Dimension::length(topbar_height(&theme)),
            "docked, it takes the bar's strip"
        );
        assert!(!space.take_structure_dirty());
        // Shown over a fullscreen window: the window keeps its whole size.
        standing.set(TopBarPresence::Shown);
        assert!(space.take_structure_dirty(), "showing moves the windows");
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        assert_eq!(
            taffy.style(node).unwrap().display,
            Display::None,
            "shown over the window, the strip is the window's"
        );
        standing.set(TopBarPresence::Hidden);
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        assert_eq!(taffy.style(node).unwrap().display, Display::None);
        standing.set(TopBarPresence::Docked);
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        assert_eq!(taffy.style(node).unwrap().display, Display::Flex);
    }

    #[test]
    fn a_laid_out_room_asks_for_no_frame() {
        let standing = Rc::new(Cell::new(TopBarPresence::Docked));
        let mut space = BarSpace::new(Rc::new(move || standing.get()));
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        space.build_layout(&mut taffy, &Theme::default(), &mut nodes);
        assert!(
            !(space.needs_redraw() || space.needs_animation_frame()),
            "it draws nothing, so a settled desk has nothing to draw here"
        );
    }
}
