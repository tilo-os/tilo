//! The bar's room in the desktop's column, for a desktop that draws its
//! bar elsewhere (the shell hosts the bar as an app over the desktop).

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{BarStanding, bar_style};
use crate::chrome::TopBarPresence;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// Whether a fullscreen window owns the bar's display, and how the bar
/// stands: what its room in the column follows.
pub type Standing = Rc<dyn Fn() -> (bool, TopBarPresence)>;

/// Takes the room [`super::TopBarChrome`] takes in the desktop's column
/// (none under a fullscreen window, none while the person hides the bar,
/// else the bar's height) and draws nothing.
pub struct BarSpace {
    standing: Standing,
    laid_out: Option<(bool, BarStanding)>,
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

    fn now(&self) -> (bool, BarStanding) {
        let (fullscreen, presence) = (self.standing)();
        (
            fullscreen,
            BarStanding::of(!fullscreen && presence.overlays()),
        )
    }

    fn stale(&self) -> bool {
        self.laid_out != Some(self.now())
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
        let (fullscreen, standing) = now;
        let style = bar_style(fullscreen, standing == BarStanding::OverColumn, theme);
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

    /// Docking or hiding the bar moves the windows, so the tree it was laid
    /// out in is rebuilt.
    fn take_structure_dirty(&mut self) -> bool {
        self.stale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn the_room_follows_fullscreen_and_presence() {
        let standing = Rc::new(Cell::new((false, TopBarPresence::Docked)));
        let read = standing.clone();
        let mut space = BarSpace::new(Rc::new(move || read.get()));
        let theme = Theme::default();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        let style = taffy.style(node).unwrap().clone();
        assert_eq!(style.display, taffy::Display::Flex);
        assert!(!space.take_structure_dirty());
        standing.set((false, TopBarPresence::Hidden));
        assert!(space.take_structure_dirty(), "hiding moves the windows");
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        assert_eq!(
            taffy.style(node).unwrap().position,
            taffy::style::Position::Absolute
        );
        standing.set((true, TopBarPresence::Hidden));
        let node = space.build_layout(&mut taffy, &theme, &mut nodes);
        assert_eq!(taffy.style(node).unwrap().display, taffy::Display::None);
    }
}
