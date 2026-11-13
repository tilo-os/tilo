//! `Facts`: an ordered row of separate facts, each an optional mark and a text
//! (`docs/design-language.md`, "Primitives"; "Words and structure").

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Tone, Widget};

#[path = "facts_component.rs"]
mod component;
pub use component::{FactProps, FactsProps};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactMark {
    /// A glyph from the icon set: the kind of thing the fact names.
    Icon(Icon),
    /// A dot in a tone: something happening now, in the colour of whoever
    /// or whatever it is about.
    Dot(Tone),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub mark: Option<FactMark>,
    pub text: String,
    pub priority: u8,
}

impl Fact {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            mark: None,
            text: text.into(),
            priority: 0,
        }
    }

    #[must_use]
    pub fn with_icon(icon: Icon, text: impl Into<String>) -> Self {
        Self {
            mark: Some(FactMark::Icon(icon)),
            text: text.into(),
            priority: 0,
        }
    }

    /// A fact that is happening now, marked by a dot in `tone`'s colour.
    #[must_use]
    pub fn dot(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            mark: Some(FactMark::Dot(tone)),
            text: text.into(),
            priority: 0,
        }
    }

    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

struct FactsMetrics {
    style: TextStyle,
    icon: f64,
    dot: f64,
    icon_gap: f64,
    gap: f64,
}

impl FactsMetrics {
    fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            style: theme.text(TextRole::Caption),
            icon: f64::from(theme.tokens.metrics.icon_sm),
            dot: f64::from(theme.tokens.spacing.s2),
            icon_gap: f64::from(rhythm.tight),
            gap: f64::from(rhythm.sibling),
        }
    }

    fn mark_width(&self, mark: Option<FactMark>) -> f64 {
        match mark {
            Some(FactMark::Icon(_)) => self.icon + self.icon_gap,
            Some(FactMark::Dot(_)) => self.dot + self.icon_gap,
            None => 0.0,
        }
    }

    fn fact_width(&self, fact: &Fact, text: &mut TextRenderer) -> f64 {
        let label = f64::from(text.measure_style(&fact.text, None, &self.style).0);
        self.mark_width(fact.mark) + label
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FactsFace<'a> {
    facts: &'a [Fact],
}

impl<'a> FactsFace<'a> {
    #[must_use]
    pub fn new(facts: &'a [Fact]) -> Self {
        Self { facts }
    }

    #[must_use]
    pub fn natural_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = FactsMetrics::of(theme);
        let shown: Vec<usize> = (0..self.facts.len()).collect();
        Self::width_of(&m, self.facts, &shown, text)
    }

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
            match fact.mark {
                Some(FactMark::Icon(icon)) => {
                    icon.draw(
                        ctx.scene,
                        ctx.transform,
                        Rect::new(x, cy - m.icon / 2.0, x + m.icon, cy + m.icon / 2.0),
                        ink,
                    );
                    x += m.icon + m.icon_gap;
                }
                Some(FactMark::Dot(tone)) => {
                    ctx.scene.fill(
                        vello::peniko::Fill::NonZero,
                        ctx.transform,
                        tone.colour(theme),
                        None,
                        &Circle::new(Point::new(x + m.dot / 2.0, cy), m.dot / 2.0),
                    );
                    x += m.dot + m.icon_gap;
                }
                None => {}
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
    layout_dirty: bool,
    layout_theme: Theme,
}

impl Facts {
    /// The facts it shows changed where it stands: the row takes the new ones and keeps its
    /// place, measuring again for what they say.
    pub fn set_facts(&mut self, facts: Vec<Fact>) {
        let same = self.items.len() == facts.len()
            && self.items.iter().zip(&facts).all(|(was, now)| {
                was.mark == now.mark && was.text == now.text && was.priority == now.priority
            });
        if same {
            return;
        }
        self.items = facts;
        self.dirty = true;
        self.layout_dirty = true;
    }

    #[must_use]
    pub fn new(facts: Vec<Fact>) -> Self {
        Self {
            items: facts,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
            layout_theme: Theme::dark(),
        }
    }
}

impl Widget for Facts {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.layout_theme != *theme {
            self.layout_theme = theme.clone();
            self.dirty = true;
            self.layout_dirty = true;
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
        // The solver rounds every edge onto the display's device pixels, so a row measured at
        // its text's exact width is handed a box up to a pixel narrower than that and the last
        // fact elides a letter it has the room to draw ("1 he…" for "1 here").
        let grid = crate::widget::device_scale();
        let width = if grid > 0.0 {
            crate::num::f32_of_f64((f64::from(width) * f64::from(grid)).ceil() / f64::from(grid))
        } else {
            width
        };
        Size {
            width: known.width.unwrap_or(width),
            height: known
                .height
                .unwrap_or(crate::num::f32_of_f64(face.height(text, theme))),
        }
    }

    crate::rect_geometry!(measured);

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
        assert_eq!(face.shown_in(10.0, &mut text, &theme), vec![0]);
    }

    /// A row of facts standing beside the chips it shares its line with is measured at its
    /// text's exact width, and the solver that places it rounds every edge onto the display's
    /// device pixels.
    #[test]
    fn a_row_measured_for_its_text_is_not_handed_a_box_that_shaves_it() {
        let theme = Theme::light();
        let mut text = TextRenderer::new();
        let m = FactsMetrics::of(&theme);
        for fact in [
            Fact::with_icon(Icon::Users, "1 here"),
            Fact::with_icon(Icon::Users, "3 here"),
            Fact::text("12 files"),
        ] {
            let natural = crate::num::f32_of_f64(
                FactsFace::new(std::slice::from_ref(&fact)).natural_width(&mut text, &theme),
            );
            let mut row = Facts::new(vec![fact.clone()]);
            let measured = row.measure(
                Size {
                    width: None,
                    height: None,
                },
                Size {
                    width: taffy::AvailableSpace::Definite(1000.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut text,
            );
            let grid = f64::from(crate::widget::device_scale());
            let boxed = f64::from(measured.width) * grid;
            let boxed = boxed.round() / grid;
            let inset = m.mark_width(fact.mark);
            let room = crate::num::f32_of_f64(boxed - inset);
            assert_eq!(
                text.elide_to_width_style(&fact.text, room, &m.style),
                fact.text,
                "{:?} measured {natural} stands in a box of {boxed}",
                fact.text
            );
        }
    }

    /// Something happening now is marked by a dot in its own colour, in the slot an icon takes: the
    /// same room, and the words after it read as they do anywhere.
    #[test]
    fn a_live_fact_is_a_dot_in_its_tone_before_its_words() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let m = FactsMetrics::of(&theme);
        let hue = crate::tokens::PresenceHue::Orange;
        let live = Fact::dot(Tone::Presence(hue), "Working now");
        assert_eq!(live.mark, Some(FactMark::Dot(Tone::Presence(hue))));
        let face = FactsFace::new(std::slice::from_ref(&live));
        let label = f64::from(text.measure_style(&live.text, None, &m.style).0);
        assert!(
            (face.natural_width(&mut text, &theme) - (m.dot + m.icon_gap + label)).abs() < 0.01,
            "the dot stands in the icon's slot"
        );
        assert_eq!(face.spoken(), vec!["Working now"], "the dot says nothing");
        assert_eq!(
            Tone::Presence(hue).colour(&theme),
            theme.tokens.colors.presence.hue(hue),
            "it wears the presence colour of whoever it is about"
        );
    }

    #[test]
    fn facts_are_separate_items_with_no_separator_glyph() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let facts = facts();
        let face = FactsFace::new(&facts);
        assert_eq!(face.spoken(), vec!["12 files", "3.4 MB", "Edited today"]);
        let m = FactsMetrics::of(&theme);
        let parts: f64 = facts.iter().map(|f| m.fact_width(f, &mut text)).sum();
        let gaps = 2.0 * f64::from(theme.tokens.spacing.rhythm.sibling);
        assert!((face.natural_width(&mut text, &theme) - parts - gaps).abs() < 0.01);
    }
}
