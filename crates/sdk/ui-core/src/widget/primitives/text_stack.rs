//! `TextStack`: a title over a secondary line, with an optional tertiary line, in fixed roles
//! and at fixed gaps (`docs/design-language.md`, "Primitives").

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Tone, Widget};

#[path = "text_stack_component.rs"]
mod component;
pub use component::TextStackProps;

fn line_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.tight) / 2.0
}

/// One line of words in a role, measured and centred on a point or aligned in a box: the bar's
/// clock reading, a live activity's headline, an overlay's timer.
#[derive(Clone, Copy, Debug)]
pub struct TextLineFace<'a> {
    text: &'a str,
    style: &'a TextStyle,
    role: TextRole,
    weight: Option<TextWeight>,
    align: TextLineAlign,
}

/// Where a [`TextLineFace`] stands in the rect it is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextLineAlign {
    /// Centred on the rect's middle line and middle column.
    #[default]
    Center,
    /// Starting at the rect's leading edge, centred on its middle line.
    Start,
}

impl<'a> TextLineFace<'a> {
    #[must_use]
    pub fn styled(text: &'a str, style: &'a TextStyle, role: TextRole) -> Self {
        Self {
            text,
            style,
            role,
            weight: None,
            align: TextLineAlign::Center,
        }
    }

    /// Set it in `weight` rather than its style's own.
    #[must_use]
    pub fn weight(mut self, weight: TextWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    #[must_use]
    pub fn start(mut self) -> Self {
        self.align = TextLineAlign::Start;
        self
    }

    fn resolved(&self) -> TextStyle {
        match self.weight {
            Some(weight) => self.style.clone().with_weight(weight),
            None => self.style.clone(),
        }
    }

    #[must_use]
    pub fn measure(&self, text: &mut TextRenderer) -> (f64, f64) {
        let style = self.resolved();
        let (w, h) = text.measure_style(self.text, None, &style);
        (f64::from(w), f64::from(h))
    }

    /// Paint the line in `rect`, in `ink`, elided to the rect's width. The
    /// `role` the face was built with is what its style already carries, so
    /// nothing is scaled twice.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ink: vello::peniko::Color) {
        let _ = self.role;
        let style = self.resolved();
        if self.text.is_empty() || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let room = crate::num::f32_of_f64(rect.width());
        let shown = ctx.text.elide_to_width_style(self.text, room, &style);
        let (w, h) = ctx.text.measure_style(&shown, None, &style);
        let x = match self.align {
            TextLineAlign::Center => rect.center().x - f64::from(w) / 2.0,
            TextLineAlign::Start => rect.x0,
        };
        ctx.text.draw_style(
            ctx.scene,
            &shown,
            ink,
            ctx.transform
                .pre_translate((x, rect.center().y - f64::from(h) / 2.0).into()),
            None,
            &style,
        );
    }
}

/// A block of wrapped text in one style, starting at its rect's leading top corner and wrapping
/// to the rect's width: a reader's body, a page's paragraph, a note's prose.
#[derive(Clone, Copy, Debug)]
pub struct TextBlockFace<'a> {
    text: &'a str,
    style: &'a TextStyle,
}

impl<'a> TextBlockFace<'a> {
    /// `text` in `style` (which carries its role's size, family and leading).
    #[must_use]
    pub fn new(text: &'a str, style: &'a TextStyle) -> Self {
        Self { text, style }
    }

    #[must_use]
    pub fn height(&self, text: &mut TextRenderer, width: f64) -> f64 {
        if self.text.is_empty() {
            return 0.0;
        }
        f64::from(
            text.measure_style(
                self.text,
                Some(crate::num::f32_of_f64(width.max(0.0))),
                self.style,
            )
            .1,
        )
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ink: vello::peniko::Color) {
        if self.text.is_empty() || rect.width() <= 0.0 {
            return;
        }
        ctx.text.draw_style(
            ctx.scene,
            self.text,
            ink,
            ctx.transform.pre_translate((rect.x0, rect.y0).into()),
            Some(crate::num::f32_of_f64(rect.width())),
            self.style,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackSize {
    /// A row's title: `Body` medium.
    #[default]
    Regular,
    /// A dense row's or a bar's title: `Label` medium.
    Dense,
}

#[derive(Clone, Copy, Debug)]
pub struct TextStackFace<'a> {
    title: &'a str,
    secondary: Option<&'a str>,
    tertiary: Option<&'a str>,
    size: StackSize,
    /// The secondary line's status ink ([`Tone::Neutral`]: secondary ink).
    secondary_tone: Tone,
    styles: Option<(&'a TextStyle, &'a TextStyle)>,
}

struct StackStyles {
    title: TextStyle,
    caption: TextStyle,
}

impl StackStyles {
    fn of(face: &TextStackFace<'_>, theme: &Theme) -> Self {
        if let Some((title, caption)) = face.styles {
            return Self {
                title: title.clone(),
                caption: caption.clone(),
            };
        }
        let role = match face.size {
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
            secondary_tone: Tone::Neutral,
            styles: None,
        }
    }

    /// Set the title and the caption lines in `title` and `caption`, for a
    /// surface that scales its type itself.
    #[must_use]
    pub fn styled(mut self, title: &'a TextStyle, caption: &'a TextStyle) -> Self {
        self.styles = Some((title, caption));
        self
    }

    /// The secondary line says a status (a playback error): it reads in
    /// `tone`'s ink.
    #[must_use]
    pub fn secondary_tone(mut self, tone: Tone) -> Self {
        self.secondary_tone = tone;
        self
    }

    #[must_use]
    pub fn secondary(mut self, text: &'a str) -> Self {
        self.secondary = (!text.is_empty()).then_some(text);
        self
    }

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

    #[must_use]
    pub fn lines(&self) -> usize {
        1 + usize::from(self.secondary.is_some()) + usize::from(self.tertiary.is_some())
    }

    #[must_use]
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let styles = StackStyles::of(self, theme);
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
        let styles = StackStyles::of(self, theme);
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
        let styles = StackStyles::of(self, theme);
        let title = f64::from(text.measure_style("Ag", None, &styles.title).1);
        rect.y0 + (rect.height() - self.height(text, theme)) / 2.0 + title / 2.0
    }

    /// The baseline-side top of the title as [`Self::paint`] places it in
    /// `rect`: where a quiet line beside the title starts.
    #[must_use]
    pub fn title_top(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> f64 {
        rect.y0 + (rect.height() - self.height(text, theme)) / 2.0
    }

    /// Paint the stack in `rect`, centred vertically, each line elided to
    /// the rect's width. Inks are measured on `ctx.theme`'s ground, so a
    /// stack on a selected row or a floating panel reads.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let styles = StackStyles::of(self, theme);
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
            let ink = match self.secondary_tone {
                Tone::Neutral => colors.text_dim,
                tone => tone.colour(theme),
            };
            y += line(ctx, secondary, &styles.caption, ink, y) + line_gap(theme);
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

pub struct TextStack {
    title: String,
    secondary: String,
    tertiary: String,
    size: StackSize,
    secondary_tone: Tone,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
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
            secondary_tone: Tone::Neutral,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
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

    pub fn set_lines(&mut self, title: &str, secondary: &str, tertiary: &str) {
        if self.title != title || self.secondary != secondary || self.tertiary != tertiary {
            title.clone_into(&mut self.title);
            secondary.clone_into(&mut self.secondary);
            tertiary.clone_into(&mut self.tertiary);
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    /// Set the secondary line's status ink
    /// ([`TextStackFace::secondary_tone`]).
    pub fn set_secondary_tone(&mut self, tone: Tone) {
        if self.secondary_tone != tone {
            self.secondary_tone = tone;
            self.dirty = true;
        }
    }

    fn face(&self) -> TextStackFace<'_> {
        let face = TextStackFace::new(&self.title)
            .secondary(&self.secondary)
            .tertiary(&self.tertiary)
            .secondary_tone(self.secondary_tone);
        match self.size {
            StackSize::Regular => face,
            StackSize::Dense => face.dense(),
        }
    }
}

impl Widget for TextStack {
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

    crate::rect_geometry!(measured);

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
        assert!(one.dense().height(&mut text, &theme) < h1);
    }
}
