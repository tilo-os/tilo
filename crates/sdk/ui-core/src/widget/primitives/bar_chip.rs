//! `BarChip`: one bar control that holds a face stack, a label and a press
//! (`docs/design-language.md`, "Primitives"; `docs/architecture/ui-inventory.md`, "Bar chip").

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::{Face, FaceStack};
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::BarControlFace;

/// One bar chip's content and state, borrowed. Built by the caller each
/// frame; [`Self::paint`] draws it and hands back where its parts landed.
#[derive(Clone, Copy, Debug)]
pub struct BarChipFace<'a> {
    name: &'a str,
    label: &'a str,
    faces: &'a [Face<'a>],
    /// How many people stand past the shown faces, counted by the overflow badge.
    overflow: usize,
    icons: &'a [Icon],
    trailing: Option<Icon>,
    tone: Option<Color>,
    open: bool,
    hovered: bool,
    pressed: bool,
    focused: bool,
    /// The label may grow to this width before it elides; `None`: as wide as the room the chip is given.
    label_width: Option<f64>,
    /// The chip's height; `None` uses the theme's bar control height.
    height: Option<f64>,
}

/// Where a [`BarChipFace`] painted its parts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BarChipLayout {
    pub chip: Rect,
    pub faces: Rect,
    /// The overflow badge's disc (zero when nobody stands past the faces).
    pub badge: Rect,
    pub label: Rect,
}

impl<'a> BarChipFace<'a> {
    /// A chip named `name` for assistive technology, standing for `faces`.
    #[must_use]
    pub fn new(name: &'a str, faces: &'a [Face<'a>]) -> Self {
        Self {
            name,
            label: "",
            faces,
            overflow: 0,
            icons: &[],
            trailing: None,
            tone: None,
            open: false,
            hovered: false,
            pressed: false,
            focused: false,
            label_width: None,
            height: None,
        }
    }

    /// Paint and measure at `height` rather than the theme's bar control
    /// height: a bar whose own controls are a little shorter than the
    /// toolkit's default keeps one top and bottom edge across every control.
    #[must_use]
    pub fn at_height(mut self, height: f64) -> Self {
        self.height = Some(height.max(1.0));
        self
    }

    fn height_of(&self, theme: &Theme) -> f64 {
        self.height.unwrap_or_else(|| Self::height(theme))
    }

    /// The words beside the faces. An empty string shows none; a chip with
    /// no faces and no label is its glyph alone.
    #[must_use]
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    #[must_use]
    pub fn overflow(mut self, overflow: usize) -> Self {
        self.overflow = overflow;
        self
    }

    /// The glyphs before the faces: a watch, an invitation, or a media
    /// pill's microphone and camera. A chip with none starts its faces at
    /// its inset.
    #[must_use]
    pub fn icons(mut self, icons: &'a [Icon]) -> Self {
        self.icons = icons;
        self
    }

    #[must_use]
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    #[must_use]
    pub fn tone(mut self, tone: Color) -> Self {
        self.tone = Some(tone);
        self
    }

    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self
    }

    /// Keyboard focus; the ring shows only while focus is visible.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn label_width(mut self, width: f64) -> Self {
        self.label_width = Some(width.max(0.0));
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        BarControlFace::height(theme)
    }

    fn pad(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.related)
    }

    fn gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.tight)
    }

    fn icon_size(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_control)
    }

    fn label_style(theme: &Theme) -> TextStyle {
        theme.text(TextRole::Label).with_weight(TextWeight::Medium)
    }

    /// The width the chip asks for its content: its pad each side, the
    /// icon, the face stack (whose rearmost face overlaps the gap), the
    /// label, and a trailing glyph, each a gap apart.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let h = self.height_of(theme);
        let gap = Self::gap(theme);
        let icon = Self::icon_size(theme);
        if self.label.is_empty()
            && self.faces.is_empty()
            && self.overflow == 0
            && self.trailing.is_none()
        {
            return h;
        }
        let mut w = 2.0 * Self::pad(theme);
        w += f64::from(u32::try_from(self.icons.len()).unwrap_or(u32::MAX)) * (icon + gap);
        let stack = self.stack_width(theme);
        if stack > 0.0 {
            w += stack;
            if !self.label.is_empty() {
                w += gap;
            }
        }
        if !self.label.is_empty() {
            let style = Self::label_style(theme);
            let measured = f64::from(text.measure_style(self.label, None, &style).0);
            w += self.label_width.map_or(measured, |room| measured.min(room));
        }
        if self.trailing.is_some() {
            w += gap + icon;
        }
        w.ceil().max(h)
    }

    fn stack_width(&self, theme: &Theme) -> f64 {
        FaceStack::in_face(self.height_of(theme)).width(self.faces.len() + self.overflow)
    }

    /// The chip as a page shows it ([`crate::composition::Face`]), on `ground`, in the inks and
    /// geometry [`Self::paint`] paints it with; `None` for a chip of faces or of more than one
    /// glyph, which shows as its paint.
    pub(crate) fn composed(
        &self,
        theme: &Theme,
        ground: Color,
    ) -> Option<crate::composition::Face> {
        use crate::composition::{Face, TextLook, rgba8};
        if !self.faces.is_empty() || self.overflow > 0 || self.icons.len() > 1 {
            return None;
        }
        let colors = &theme.tokens.colors;
        let control = BarControlFace::new(self.name)
            .open(self.open)
            .pressed(self.pressed)
            .hovered(self.hovered)
            .focused(self.focused);
        let lift = control.fill(theme, ground);
        let fill = match (lift, self.tone) {
            (Some(lift), Some(tone)) => Some(over(tone.multiply_alpha(0.15), lift)),
            (None, Some(tone)) => Some(tone.multiply_alpha(0.15)),
            (lift, None) => lift,
        };
        let lit = self.open || self.hovered || self.pressed;
        let ink = if lit {
            colors.ink_on(ground, colors.text_primary, 4.5)
        } else {
            colors.ink_on(ground, colors.text_dim, 3.0)
        };
        let label_ink = colors.ink_on(ground, colors.text_primary, 4.5);
        let icon = Self::icon_size(theme);
        let leading = match self.icons.first() {
            Some(i) => Some(i.glyph(icon, ink, false)?),
            None => None,
        };
        let trailing = match self.trailing {
            Some(i) => Some(i.glyph(icon, ink, false)?),
            None => None,
        };
        let alone = self.label.is_empty() && self.trailing.is_none();
        Some(Face {
            icon: leading,
            label: (!self.label.is_empty()).then(|| {
                TextLook::new(
                    self.label.to_owned(),
                    &Self::label_style(theme),
                    Some(label_ink),
                )
                .with_max_lines(Some(1))
            }),
            detail: None,
            trailing,
            fill: fill.map(rgba8),
            edge: None,
            radius: f64::from(theme.tokens.radii.control),
            pad: if alone { 0.0 } else { Self::pad(theme) },
            gap: Self::gap(theme),
            spread: false,
        })
    }

    /// Paint the chip in `rect` (its width from [`Self::width`], its height
    /// [`Self::height`]) standing on `ground`.
    #[must_use]
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ground: Color) -> BarChipLayout {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return BarChipLayout::default();
        }
        let control = BarControlFace::new(self.name)
            .open(self.open)
            .pressed(self.pressed)
            .hovered(self.hovered)
            .focused(self.focused);
        if let Some(fill) = control.fill(theme, ground) {
            let radius = f64::from(theme.tokens.radii.control).min(rect.height() / 2.0);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, radius),
            );
        }
        // A tone is a faint wash inside the control's own lift, never a
        // slab: it says what the chip is about, not what it is.
        if let Some(tone) = self.tone {
            let radius = f64::from(theme.tokens.radii.control).min(rect.height() / 2.0);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                tone.multiply_alpha(0.15),
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, radius),
            );
        }
        let lit = self.open || self.hovered || self.pressed;
        let ink = if lit {
            colors.ink_on(ground, colors.text_primary, 4.5)
        } else {
            colors.ink_on(ground, colors.text_dim, 3.0)
        };
        let label_ink = colors.ink_on(ground, colors.text_primary, 4.5);
        let gap = Self::gap(theme);
        let icon = Self::icon_size(theme);
        let cy = rect.center().y;
        let mut layout = BarChipLayout {
            chip: rect,
            ..BarChipLayout::default()
        };
        let alone = self.label.is_empty()
            && self.faces.is_empty()
            && self.overflow == 0
            && self.trailing.is_none()
            && self.icons.len() == 1;
        let mut x = if alone {
            rect.x0
        } else {
            rect.x0 + Self::pad(theme)
        };
        for glyph in self.icons {
            let centre = if alone {
                rect.center()
            } else {
                Point::new(x + icon / 2.0, cy)
            };
            glyph.draw_with_stroke(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(centre, (icon, icon)),
                ink,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            x += icon + gap;
        }
        let stack = self.stack_width(theme);
        if stack > 0.0 {
            let right = x + stack;
            let (rects, badge) = FaceStack::in_face(self.height_of(theme)).paint(
                ctx,
                (right, cy),
                (self.faces, self.overflow),
                ground,
            );
            layout.faces = rects
                .iter()
                .copied()
                .reduce(|a, b| a.union(b))
                .unwrap_or(Rect::ZERO);
            layout.badge = badge.unwrap_or(Rect::ZERO);
            x = right;
            if !self.label.is_empty() {
                x += gap;
            }
        }
        let trailing = self.trailing.map_or(0.0, |_| gap + icon);
        if !self.label.is_empty() {
            let style = Self::label_style(theme);
            let room = (rect.x1 - Self::pad(theme) - trailing - x).max(0.0);
            let room = self.label_width.map_or(room, |most| room.min(most));
            let shown =
                ctx.text
                    .elide_to_width_style(self.label, crate::num::f32_of_f64(room), &style);
            let (w, h) = ctx.text.measure_style(&shown, None, &style);
            if !shown.is_empty() {
                ctx.text.draw_style(
                    ctx.scene,
                    &shown,
                    label_ink,
                    ctx.transform
                        .pre_translate((x, cy - f64::from(h) / 2.0).into()),
                    None,
                    &style,
                );
            }
            layout.label = Rect::new(
                x,
                cy - f64::from(h) / 2.0,
                x + f64::from(w),
                cy + f64::from(h) / 2.0,
            );
            x += f64::from(w);
        }
        if let Some(glyph) = self.trailing {
            glyph.draw_with_stroke(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(Point::new(x + gap + icon / 2.0, cy), (icon, icon)),
                ink,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        if self.focused {
            crate::widget::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                rect,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
        layout
    }

    /// Where a chip of `width` stands with its leading edge at `x` on a bar
    /// whose middle line is `cy`.
    #[must_use]
    pub fn place(x: f64, cy: f64, width: f64, theme: &Theme) -> Rect {
        Self::place_at(x, cy, width, Self::height(theme))
    }

    /// [`Self::place`] at `height` rather than the theme's bar control height.
    #[must_use]
    pub fn place_at(x: f64, cy: f64, width: f64, height: f64) -> Rect {
        Rect::new(x, cy - height / 2.0, x + width, cy + height / 2.0)
    }
}

/// `top` laid over an opaque `under`, as the canvas paints a wash on a lift.
fn over(top: Color, under: Color) -> Color {
    let [r, g, b, a] = top.components;
    let [ur, ug, ub, ua] = under.components;
    Color::new([
        r * a + ur * (1.0 - a),
        g * a + ug * (1.0 - a),
        b * a + ub * (1.0 - a),
        a + ua * (1.0 - a),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn faces(names: &[&'static str]) -> Vec<Face<'static>> {
        names
            .iter()
            .map(|name| Face::new(name, name, Theme::dark().tokens.colors.intent.you))
            .collect()
    }

    #[test]
    fn a_chip_names_itself_and_says_everyone_it_stands_for() {
        let people = faces(&["Ada Lovelace"]);
        let chip = BarChipFace::new("Who is here", &people);
        assert_eq!(chip.name(), "Who is here");
    }

    #[test]
    fn a_chip_hugs_its_faces_and_its_label_at_the_bar_control_height() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let people = faces(&["Ada Lovelace"]);
        let bare = BarChipFace::new("Who is here", &people);
        let labelled = bare.label("Editing");
        assert!(labelled.width(&mut text, &theme) > bare.width(&mut text, &theme));
        assert!((BarChipFace::height(&theme) - 28.0).abs() < 1e-6);
        let alone = BarChipFace::new("Watch", &[]);
        assert!((alone.width(&mut text, &theme) - 28.0).abs() < 1e-6);
    }

    #[test]
    fn a_chip_paints_no_ground_at_rest_and_lifts_under_the_pointer() {
        let theme = Theme::dark();
        let ground = theme.tokens.colors.surfaces.base;
        let people = faces(&["Ada Lovelace"]);
        let paint = |chip: BarChipFace<'_>| {
            let mut scene = vello::Scene::new();
            let mut text = TextRenderer::new();
            let mut ctx = PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            );
            let rect = Rect::new(0.0, 0.0, 120.0, BarChipFace::height(&theme));
            let _ = chip.paint(&mut ctx, rect, ground);
            scene.encoding().draw_tags.len()
        };
        let rest = paint(BarChipFace::new("Who is here", &people));
        let hovered = paint(BarChipFace::new("Who is here", &people).hovered(true));
        assert!(hovered > rest, "a hover adds the control's lift");
    }
}
