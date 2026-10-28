//! The retained parts of a column header the shell owns itself: the title
//! (with its count). The actions that do not fit overflow into the toolkit's
//! one door ([`crate::widget::OverflowDoor`]).

use crate::num::f32_of_f64;
use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::chrome::header_row;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

/// A column's title in the header row: `Heading`, and an optional count in
/// a quiet pill after it. Measured by the host, so the actions after it
/// start where the title ends.
pub(super) struct HeaderTitle {
    pub(super) title: Option<String>,
    pub(super) count: Option<usize>,
    theme: Theme,
    rect: Rect,
    /// The title and count the last measure saw; a change needs another.
    measured: Option<(Option<String>, Option<usize>)>,
    /// The title, count and place the last draw painted; a change needs
    /// another. `None` until drawn in the current theme.
    painted: Option<(Option<String>, Option<usize>, Rect)>,
}

impl HeaderTitle {
    pub(super) fn new() -> Self {
        Self {
            title: None,
            count: None,
            theme: Theme::dark(),
            rect: Rect::ZERO,
            measured: None,
            painted: None,
        }
    }

    pub(super) fn is_shown(&self) -> bool {
        self.title.is_some()
    }
}

impl Widget for HeaderTitle {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        self.measured = None;
        self.painted = None;
        let style = Style {
            flex_shrink: 1.0,
            min_size: Size::from_lengths(0.0, 0.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.measured = Some((self.title.clone(), self.count));
        let Some(title) = self.title.as_deref() else {
            return taffy::Size::ZERO;
        };
        let (w, h) = header_row::measure_title(text, &self.theme, title, self.count);
        taffy::Size {
            width: known.width.unwrap_or(f32_of_f64(w.ceil())),
            height: known.height.unwrap_or(f32_of_f64(h.ceil())),
        }
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if let Some(title) = self.title.as_deref() {
            header_row::paint_title_in(scene, transform, text, theme, self.rect, title, self.count);
        }
    }

    fn name(&self) -> &'static str {
        "NavShellTitle"
    }

    /// A title that shows what it painted needs no frame: a pointer moving
    /// over the window repaints nothing here.
    fn needs_redraw(&self) -> bool {
        self.painted.as_ref().is_none_or(|(title, count, rect)| {
            *title != self.title || *count != self.count || *rect != self.rect
        })
    }

    fn mark_drawn(&mut self) {
        self.painted = Some((self.title.clone(), self.count, self.rect));
    }

    fn is_layout_dirty(&self) -> bool {
        self.measured
            .as_ref()
            .is_none_or(|(title, count)| *title != self.title || *count != self.count)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let title = self.title.clone()?;
        let mut node = AccessNode::new(Role::Heading { level: 2 }).with_name(title);
        if let Some(count) = self.count {
            node = node.with_value(count.to_string());
        }
        Some(node)
    }
}
