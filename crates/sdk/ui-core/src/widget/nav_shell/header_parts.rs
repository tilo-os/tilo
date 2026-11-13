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
    /// The window's sidebar stands beside the content drawing this same count for the place
    /// chosen in it: the header leaves the number to the place.
    pub(super) repeated_by_sidebar: bool,
    /// The content shows this same title in a heading of its own, on screen right now: the
    /// header keeps its place in the row but draws nothing, so one title is never twice.
    pub(super) covered: bool,
    /// The content edits the page's title in a field and shows no heading of it: the sheet draws
    /// the title once — the field the person types in — and the header's heading stands where
    /// the field does, saying the words the field holds, so the page keeps its name.
    pub(super) edited: Option<super::TitleShown>,
    theme: Theme,
    rect: Rect,
    /// The title, count, cover and edited place the last measure saw; a change needs another.
    measured: Option<(
        Option<String>,
        Option<usize>,
        bool,
        Option<super::TitleShown>,
    )>,
    /// The title, count, place and cover the last draw painted; a change needs
    /// another. `None` until drawn in the current theme.
    painted: Option<(Option<String>, Option<usize>, Rect, bool)>,
}

impl HeaderTitle {
    pub(super) fn new() -> Self {
        Self {
            title: None,
            count: None,
            repeated_by_sidebar: false,
            covered: false,
            edited: None,
            theme: Theme::dark(),
            rect: Rect::ZERO,
            measured: None,
            painted: None,
        }
    }

    pub(super) fn is_shown(&self) -> bool {
        self.title.is_some()
    }

    pub(super) fn is_covered(&self) -> bool {
        self.covered
    }

    /// Take the row's title away while `covered`, or give it back. The row keeps its height
    /// either way: it is the shell's, not the title's.
    pub(super) fn set_covered(&mut self, covered: bool) {
        self.covered = covered;
    }

    /// Where the field that edits the page's title stands and what it says, while the page shows
    /// the title in a field and no heading of it: the header draws nothing there, and its heading
    /// node stands where the field does, saying the field's words, so the page keeps its name.
    pub(super) fn set_edited(&mut self, edited: Option<super::TitleShown>) {
        self.edited = edited;
    }

    /// The count the header draws: none while the place beside it already shows it, and none
    /// for zero, which wants no attention.
    fn shown_count(&self) -> Option<usize> {
        self.count
            .filter(|count| *count > 0 && !self.repeated_by_sidebar)
    }
}

impl Widget for HeaderTitle {
    /// The page's title as a heading of the bar, unless the page shows it
    /// itself.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("h2");
        let Some(title) = self.title.as_deref().filter(|_| !self.covered) else {
            return;
        };
        let theme = cx.theme();
        let look = crate::composition::TextLook::new(
            title.to_owned(),
            &crate::chrome::header_row::title_style(theme),
            Some(theme.tokens.colors.text_primary),
        )
        .with_max_lines(Some(1));
        cx.text(look);
    }

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
        let count = self.shown_count();
        self.measured = Some((self.title.clone(), count, self.covered, self.edited.clone()));
        let Some(title) = self.title.as_deref() else {
            return taffy::Size::ZERO;
        };
        if self.covered {
            return taffy::Size::ZERO;
        }
        let (w, h) = header_row::measure_title(text, &self.theme, title, count);
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
        if self.covered || self.rect.width() <= 0.0 {
            return;
        }
        if let Some(title) = self.title.as_deref() {
            let count = self.shown_count();
            header_row::paint_title_in(scene, transform, text, theme, self.rect, title, count);
        }
    }

    fn name(&self) -> &'static str {
        "NavShellTitle"
    }

    /// A title that shows what it painted needs no frame: a pointer moving
    /// over the window repaints nothing here.
    fn needs_redraw(&self) -> bool {
        let count = self.shown_count();
        self.painted
            .as_ref()
            .is_none_or(|(title, painted, rect, covered)| {
                *title != self.title
                    || *painted != count
                    || *rect != self.rect
                    || *covered != self.covered
            })
    }

    fn mark_drawn(&mut self) {
        self.painted = Some((
            self.title.clone(),
            self.shown_count(),
            self.rect,
            self.covered,
        ));
    }

    fn is_layout_dirty(&self) -> bool {
        let count = self.shown_count();
        self.measured
            .as_ref()
            .is_none_or(|(title, measured, covered, edited)| {
                *title != self.title
                    || *measured != count
                    || *covered != self.covered
                    || *edited != self.edited
            })
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        if self.covered && self.edited.is_none() {
            return None;
        }
        if let Some(shown) = &self.edited {
            let node = AccessNode::new(Role::Heading { level: 2 })
                .with_name(shown.words.clone())
                .with_bounds(shown.rect);
            return Some(node);
        }
        let title = self.title.clone()?;
        let mut node = AccessNode::new(Role::Heading { level: 2 }).with_name(title);
        if let Some(count) = self.shown_count() {
            node = node.with_value(count.to_string());
        }
        Some(node)
    }
}
