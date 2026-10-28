//! `Facts`: an ordered row of separate facts, each an optional icon and a
//! text (`docs/design-language.md`, "Primitives"; "Words and structure").
//!
//! Facts are separate components laid out with space between them: a
//! `related` gap between a fact's icon and its text and a `sibling` gap
//! between facts. Nothing is joined into a string and there is no separator
//! glyph — "12 files · 3 MB · Edited today" is three facts. When the row is
//! too narrow, the lowest-priority fact goes first; the last one left
//! elides its text.
//!
//! [`FactsFace`] is the immediate painter; [`Facts`] the retained widget.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// One fact: an optional icon from the icon set and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub icon: Option<Icon>,
    pub text: String,
    /// Higher stays longer when the row is narrow. Facts of equal priority
    /// go last-first.
    pub priority: u8,
}

impl Fact {
    /// A fact with no icon, at the default priority.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            icon: None,
            text: text.into(),
            priority: 0,
        }
    }

    #[must_use]
    pub fn with_icon(icon: Icon, text: impl Into<String>) -> Self {
        Self {
            icon: Some(icon),
            text: text.into(),
            priority: 0,
        }
    }

    /// Keep this fact longer than lower-priority ones.
    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

/// Geometry and type of a row of facts in one theme.
struct FactsMetrics {
    style: TextStyle,
    icon: f64,
    icon_gap: f64,
    gap: f64,
}

impl FactsMetrics {
    fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            style: theme.text(TextRole::Caption),
            icon: f64::from(theme.tokens.metrics.icon_sm),
            icon_gap: f64::from(rhythm.tight),
            gap: f64::from(rhythm.sibling),
        }
    }

    fn fact_width(&self, fact: &Fact, text: &mut TextRenderer) -> f64 {
        let label = f64::from(text.measure_style(&fact.text, None, &self.style).0);
        let icon = if fact.icon.is_some() {
            self.icon + self.icon_gap
        } else {
            0.0
        };
        icon + label
    }
}

/// A row of facts, borrowed.
#[derive(Clone, Copy, Debug)]
pub struct FactsFace<'a> {
    facts: &'a [Fact],
}

impl<'a> FactsFace<'a> {
    #[must_use]
    pub fn new(facts: &'a [Fact]) -> Self {
        Self { facts }
    }

    /// The width every fact takes at its natural size.
    #[must_use]
    pub fn natural_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = FactsMetrics::of(theme);
        let shown: Vec<usize> = (0..self.facts.len()).collect();
        Self::width_of(&m, self.facts, &shown, text)
    }

    /// The row's height: one caption line, or the icon if taller.
    #[must_use]
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = FactsMetrics::of(theme);
        f64::from(text.measure_style("Ag", None, &m.style).1).max(m.icon)
    }

    fn width_of(m: &FactsMetrics, facts: &[Fact], shown: &[usize], text: &mut TextRenderer) -> f64 {
        let widths: f64 = shown
            .iter()
            .filter_map(|&i| facts.get(i))
            .map(|fact| m.fact_width(fact, text))
            .sum();
        #[allow(clippy::cast_precision_loss, reason = "a row holds a handful of facts")]
        let gaps = shown.len().saturating_sub(1) as f64 * m.gap;
        widths + gaps
    }

    /// The indices of the facts shown in `width`, in order: the
    /// lowest-priority (and, among equals, the last) dropped first until
    /// the rest fit. At least one is always shown; it elides if it must.
    #[must_use]
    pub fn shown_in(&self, width: f64, text: &mut TextRenderer, theme: &Theme) -> Vec<usize> {
        let m = FactsMetrics::of(theme);
        let mut shown: Vec<usize> = (0..self.facts.len()).collect();
        while shown.len() > 1 && Self::width_of(&m, self.facts, &shown, text) > width {
            let drop = shown
                .iter()
                .enumerate()
                .min_by(|(ia, a), (ib, b)| {
                    let pa = self.facts.get(**a).map_or(0, |f| f.priority);
                    let pb = self.facts.get(**b).map_or(0, |f| f.priority);
                    pa.cmp(&pb).then(ib.cmp(ia))
                })
                .map(|(at, _)| at);
            if let Some(at) = drop {
                shown.remove(at);
            }
        }
        shown
    }

    /// Paint the facts that fit in `rect`, from its leading edge, centred
    /// on its middle line. Icons and text in secondary ink measured on
    /// `ctx.theme`'s ground. Returns the painted width.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) -> f64 {
        let theme = ctx.theme;
        let m = FactsMetrics::of(theme);
        let ink = theme.tokens.colors.text_dim;
        let shown = self.shown_in(rect.width(), ctx.text, theme);
        let line = f64::from(ctx.text.measure_style("Ag", None, &m.style).1);
        let cy = rect.center().y;
        let mut x = rect.x0;
        for (n, &i) in shown.iter().enumerate() {
            let Some(fact) = self.facts.get(i) else {
                continue;
            };
            if n > 0 {
                x += m.gap;
            }
            if let Some(icon) = fact.icon {
                icon.draw(
                    ctx.scene,
                    ctx.transform,
                    Rect::new(x, cy - m.icon / 2.0, x + m.icon, cy + m.icon / 2.0),
                    ink,
                );
                x += m.icon + m.icon_gap;
            }
            let room = crate::num::f32_of_f64((rect.x1 - x).max(0.0));
            let label = ctx.text.elide_to_width_style(&fact.text, room, &m.style);
            let w = f64::from(ctx.text.measure_style(&label, None, &m.style).0);
            ctx.text.draw_style(
                ctx.scene,
                &label,
                ink,
                ctx.transform.pre_translate((x, cy - line / 2.0).into()),
                None,
                &m.style,
            );
            x += w;
        }
        x - rect.x0
    }

    /// What a screen reader says: every fact, in order, as separate items.
    #[must_use]
    pub fn spoken(&self) -> Vec<String> {
        self.facts.iter().map(|f| f.text.clone()).collect()
    }
}

/// A retained [`FactsFace`].
pub struct Facts {
    items: Vec<Fact>,
    rect: Rect,
    dirty: bool,
    layout_theme: Theme,
}

impl Facts {
    #[must_use]
    pub fn new(facts: Vec<Fact>) -> Self {
        Self {
            items: facts,
            rect: Rect::ZERO,
            dirty: true,
            layout_theme: Theme::dark(),
        }
    }
}

impl Widget for Facts {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.layout_theme != *theme {
            self.layout_theme = theme.clone();
            self.dirty = true;
        }
        let style = Style {
            size: Size::auto(),
            flex_shrink: 1.0,
            min_size: Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let face = FactsFace::new(&self.items);
        let theme = &self.layout_theme;
        let natural = crate::num::f32_of_f64(face.natural_width(text, theme));
        let width = match available.width {
            taffy::AvailableSpace::Definite(w) => natural.min(w),
            _ => natural,
        };
        Size {
            width: known.width.unwrap_or(width),
            height: known
                .height
                .unwrap_or(crate::num::f32_of_f64(face.height(text, theme))),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        FactsFace::new(&self.items).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    crate::redraw_flag!();

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Facts"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let children = self
            .items
            .iter()
            .map(|f| AccessNode::new(Role::StaticText).with_name(f.text.clone()))
            .collect();
        Some(AccessNode::new(Role::List).with_children(children))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Vec<Fact> {
        vec![
            Fact::with_icon(Icon::File, "12 files").priority(2),
            Fact::text("3.4 MB"),
            Fact::with_icon(Icon::Clock, "Edited today").priority(1),
        ]
    }

    #[test]
    fn facts_drop_lowest_priority_first_and_keep_one() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let facts = facts();
        let face = FactsFace::new(&facts);
        let all = face.natural_width(&mut text, &theme);
        assert_eq!(face.shown_in(all, &mut text, &theme), vec![0, 1, 2]);
        // A little short: the size (priority 0) goes first.
        assert_eq!(face.shown_in(all - 1.0, &mut text, &theme), vec![0, 2]);
        // Very short: only the highest priority is left, and it elides.
        assert_eq!(face.shown_in(10.0, &mut text, &theme), vec![0]);
    }

    #[test]
    fn facts_are_separate_items_with_no_separator_glyph() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let facts = facts();
        let face = FactsFace::new(&facts);
        assert_eq!(face.spoken(), vec!["12 files", "3.4 MB", "Edited today"]);
        // The gaps are the rhythm, not a glyph: the natural width is the
        // facts plus two sibling gaps, nothing else.
        let m = FactsMetrics::of(&theme);
        let parts: f64 = facts.iter().map(|f| m.fact_width(f, &mut text)).sum();
        let gaps = 2.0 * f64::from(theme.tokens.spacing.rhythm.sibling);
        assert!((face.natural_width(&mut text, &theme) - parts - gaps).abs() < 0.01);
    }
}
