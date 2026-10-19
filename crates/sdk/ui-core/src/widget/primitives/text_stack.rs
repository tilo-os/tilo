//! `TextStack`: a title over a secondary line, with an optional tertiary
//! line, in fixed roles and at fixed gaps (`docs/design-language.md`,
//! "Primitives").
//!
//! | Line | Role | Ink |
//! |---|---|---|
//! | title | `Body` medium (`Label` medium when dense) | primary |
//! | secondary | `Caption` | secondary |
//! | tertiary | `Caption` | tertiary |
//!
//! Each line is one line, elided at its end. Two facts never share a line:
//! a fact that is not the title goes on the secondary line, or into
//! [`super::Facts`]. [`TextStackFace`] is the immediate painter a canvas
//! host or another primitive uses; [`TextStack`] is the retained widget.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// The gap between two lines of a stack: half the tight step.
fn line_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.tight) / 2.0
}

/// How prominent a stack's title is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackSize {
    /// A row's title: `Body` medium.
    #[default]
    Regular,
    /// A dense row's or a bar's title: `Label` medium.
    Dense,
}

/// A text stack's lines, borrowed, and how to paint them.
#[derive(Clone, Copy, Debug)]
pub struct TextStackFace<'a> {
    title: &'a str,
    secondary: Option<&'a str>,
    tertiary: Option<&'a str>,
    size: StackSize,
}

/// The styles and inks of a stack in one theme.
struct StackStyles {
    title: TextStyle,
    caption: TextStyle,
}

impl StackStyles {
    fn of(size: StackSize, theme: &Theme) -> Self {
        let role = match size {
            StackSize::Regular => TextRole::Body,
            StackSize::Dense => TextRole::Label,
        };
        Self {
            title: theme.text(role).with_weight(TextWeight::Medium),
            caption: theme.text(TextRole::Caption),
        }
    }
}

impl<'a> TextStackFace<'a> {
    #[must_use]
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            secondary: None,
            tertiary: None,
            size: StackSize::Regular,
        }
    }

    /// The secondary line. An empty string shows no line.
    #[must_use]
    pub fn secondary(mut self, text: &'a str) -> Self {
        self.secondary = (!text.is_empty()).then_some(text);
        self
    }

    /// The tertiary line. An empty string shows no line.
    #[must_use]
    pub fn tertiary(mut self, text: &'a str) -> Self {
        self.tertiary = (!text.is_empty()).then_some(text);
        self
    }

    /// A `Label` title, for a dense row or a bar.
    #[must_use]
    pub fn dense(mut self) -> Self {
        self.size = StackSize::Dense;
        self
    }

    /// How many lines the stack shows.
    #[must_use]
    pub fn lines(&self) -> usize {
        1 + usize::from(self.secondary.is_some()) + usize::from(self.tertiary.is_some())
    }

    /// The stack's height: its lines and the gaps between them.
    #[must_use]
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let styles = StackStyles::of(self.size, theme);
        let line = |text: &mut TextRenderer, style: &TextStyle| {
            f64::from(text.measure_style("Ag", None, style).1)
        };
        let title = line(text, &styles.title);
        let caption = line(text, &styles.caption);
        let extra = self.lines() - 1;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a stack has at most three lines"
        )]
        let extra = extra as f64;
        title + extra * (caption + line_gap(theme))
    }

    /// The widest line's natural width.
    #[must_use]
    pub fn natural_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let styles = StackStyles::of(self.size, theme);
        let mut width = f64::from(text.measure_style(self.title, None, &styles.title).0);
        for line in [self.secondary, self.tertiary].into_iter().flatten() {
            width = width.max(f64::from(text.measure_style(line, None, &styles.caption).0));
        }
        width.ceil()
    }

    /// The centre line of the title as [`Self::paint`] places it in
    /// `rect`: what a leading icon beside a multi-line stack lines up with.
    #[must_use]
    pub fn title_centre(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let styles = StackStyles::of(self.size, theme);
        let title = f64::from(text.measure_style("Ag", None, &styles.title).1);
        rect.y0 + (rect.height() - self.height(text, theme)) / 2.0 + title / 2.0
    }

    /// Paint the stack in `rect`, centred vertically, each line elided to
    /// the rect's width. Inks are measured on `ctx.theme`'s ground, so a
    /// stack on a selected row or a floating panel reads.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let styles = StackStyles::of(self.size, theme);
        let colors = &theme.tokens.colors;
        let height = self.height(ctx.text, theme);
        let mut y = rect.y0 + (rect.height() - height) / 2.0;
        let width = crate::num::f32_of_f64(rect.width().max(0.0));
        let line = |ctx: &mut PaintCtx<'_>, s: &str, style: &TextStyle, ink, y: f64| {
            let shown = ctx.text.elide_to_width_style(s, width, style);
            let h = f64::from(ctx.text.measure_style("Ag", None, style).1);
            ctx.text.draw_style(
                ctx.scene,
                &shown,
                ink,
                ctx.transform.pre_translate((rect.x0, y).into()),
                None,
                style,
            );
            h
        };
        y += line(ctx, self.title, &styles.title, colors.text_primary, y) + line_gap(theme);
        if let Some(secondary) = self.secondary {
            y += line(ctx, secondary, &styles.caption, colors.text_dim, y) + line_gap(theme);
        }
        if let Some(tertiary) = self.tertiary {
            line(ctx, tertiary, &styles.caption, colors.text_tertiary, y);
        }
    }

    /// What a screen reader says: the lines in order.
    #[must_use]
    pub fn spoken(&self) -> String {
        [Some(self.title), self.secondary, self.tertiary]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A retained [`TextStackFace`].
pub struct TextStack {
    title: String,
    secondary: String,
    tertiary: String,
    size: StackSize,
    rect: Rect,
    dirty: bool,
    /// The theme the last layout was built with, for measuring.
    layout_theme: Theme,
}

impl TextStack {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            secondary: String::new(),
            tertiary: String::new(),
            size: StackSize::Regular,
            rect: Rect::ZERO,
            dirty: true,
            layout_theme: Theme::dark(),
        }
    }

    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = text.into();
        self
    }

    #[must_use]
    pub fn tertiary(mut self, text: impl Into<String>) -> Self {
        self.tertiary = text.into();
        self
    }

    #[must_use]
    pub fn dense(mut self) -> Self {
        self.size = StackSize::Dense;
        self
    }

    /// Replace the lines.
    pub fn set_lines(&mut self, title: &str, secondary: &str, tertiary: &str) {
        if self.title != title || self.secondary != secondary || self.tertiary != tertiary {
            self.title = title.to_owned();
            self.secondary = secondary.to_owned();
            self.tertiary = tertiary.to_owned();
            self.dirty = true;
        }
    }

    fn face(&self) -> TextStackFace<'_> {
        let face = TextStackFace::new(&self.title)
            .secondary(&self.secondary)
            .tertiary(&self.tertiary);
        match self.size {
            StackSize::Regular => face,
            StackSize::Dense => face.dense(),
        }
    }
}

impl Widget for TextStack {
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
        let theme = &self.layout_theme;
        let face = self.face();
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
        let face = self.face();
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    crate::redraw_flag!();

    // Its size changes only with its lines or the theme, which mark it.
    fn is_layout_dirty(&self) -> bool {
        self.dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TextStack"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::StaticText).with_name(self.face().spoken()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_is_its_lines_at_fixed_gaps_and_elides_each() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let one = TextStackFace::new("Wi-Fi");
        let two = one.secondary("Home network");
        let three = two.tertiary("Connected yesterday");
        let (h1, h2, h3) = (
            one.height(&mut text, &theme),
            two.height(&mut text, &theme),
            three.height(&mut text, &theme),
        );
        assert!(h2 > h1 && h3 > h2);
        assert!(
            ((h3 - h2) - (h2 - h1)).abs() < 0.01,
            "every caption line adds the same step"
        );
        assert_eq!(two.lines(), 2);
        assert_eq!(TextStackFace::new("A").secondary("").lines(), 1);
        assert_eq!(three.spoken(), "Wi-Fi, Home network, Connected yesterday");
        // The dense title is shorter than the regular one.
        assert!(one.dense().height(&mut text, &theme) < h1);
    }
}
