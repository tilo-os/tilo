//! `BarControl` and `IconButton` (`docs/design-language.md`,
//! "Primitives").
//!
//! A **bar control** is what stands on the desktop's top bar and on a
//! window's bar: plain at rest, a half lift under the pointer, a whole lift
//! while pressed or while its menu is open, the control radius, 28 high.
//! It shows an icon, a label, or both, and an optional trailing icon (a
//! chevron that opens its menu). Icons rest in secondary ink and turn
//! primary under the pointer or while open; the label is primary ink.
//!
//! An **icon button** is a square [`crate::widget::Button`] whose
//! constructor requires its accessible name, which is also its tooltip.

use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonRole, ControlSize};

/// A bar control's content and state, borrowed.
#[derive(Clone, Copy, Debug)]
pub struct BarControlFace<'a> {
    name: &'a str,
    icon: Option<Icon>,
    label: Option<&'a str>,
    trailing: Option<Icon>,
    open: bool,
    hovered: bool,
    pressed: bool,
    focused: bool,
    label_role: TextRole,
}

impl<'a> BarControlFace<'a> {
    /// A bar control named `name` for assistive technology (and as its
    /// tooltip when it shows only an icon).
    #[must_use]
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
            icon: None,
            label: None,
            trailing: None,
            open: false,
            hovered: false,
            pressed: false,
            focused: false,
            label_role: TextRole::Label,
        }
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The visible label. An empty string shows none.
    #[must_use]
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = (!label.is_empty()).then_some(label);
        self
    }

    /// Set the label in `Body` (the time in the top bar) instead of
    /// `Label`.
    #[must_use]
    pub fn body_label(mut self) -> Self {
        self.label_role = TextRole::Body;
        self
    }

    /// A trailing icon: the chevron of a control that opens a menu.
    #[must_use]
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    /// Its menu or panel is open: it holds the whole lift.
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

    /// Keyboard focus. The ring shows only while focus is visible
    /// ([`crate::focus_visible`]).
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The accessible name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Every bar control's height.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.bar_control_height)
    }

    fn style(&self, theme: &Theme) -> TextStyle {
        theme.text(self.label_role)
    }

    fn icon_size(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_control)
    }

    fn pad(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.related)
    }

    /// The control's width for its content: a square for an icon alone,
    /// otherwise its content plus the related inset each side.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let h = Self::height(theme);
        let icon = Self::icon_size(theme);
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let Some(label) = self.label else {
            return match self.trailing {
                Some(_) if self.icon.is_some() => Self::pad(theme) * 2.0 + icon * 2.0 + gap,
                _ => h,
            };
        };
        let mut w = f64::from(text.measure_style(label, None, &self.style(theme)).0);
        if self.icon.is_some() {
            w += icon + gap;
        }
        if self.trailing.is_some() {
            w += icon + gap;
        }
        (w + 2.0 * Self::pad(theme)).ceil().max(h)
    }

    /// The fill behind the control on `ground` (the bar's ground: the
    /// desktop for the top bar, the window for a window bar).
    #[must_use]
    pub fn fill(&self, theme: &Theme, ground: Color) -> Option<Color> {
        let colors = &theme.tokens.colors;
        if self.open || self.pressed {
            Some(colors.lift_on(ground, 1.0))
        } else if self.hovered {
            Some(colors.lift_on(ground, 0.5))
        } else {
            None
        }
    }

    /// Paint the control in `rect` (its width from [`Self::width`], its
    /// height [`Self::height`]) standing on `ground`. Inks are measured on
    /// what is under them.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, ground: Color) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.control).min(rect.height() / 2.0);
        let fill = self.fill(theme, ground);
        if let Some(fill) = fill {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        }
        let under = fill.unwrap_or(ground);
        let lit = self.open || self.hovered || self.pressed;
        let label_ink = colors.ink_on(under, colors.text_primary, 4.5);
        let icon_ink = if lit {
            label_ink
        } else {
            colors.ink_on(under, colors.text_dim, 3.0)
        };
        let icon = Self::icon_size(theme);
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let cy = rect.center().y;
        let glyph = |x: f64| Rect::new(x, cy - icon / 2.0, x + icon, cy + icon / 2.0);
        if self.label.is_none() && self.trailing.is_none() {
            if let Some(i) = self.icon {
                i.draw(
                    ctx.scene,
                    ctx.transform,
                    Rect::from_center_size(rect.center(), (icon, icon)),
                    icon_ink,
                );
            }
        } else {
            let mut x = rect.x0 + Self::pad(theme);
            if let Some(i) = self.icon {
                i.draw(ctx.scene, ctx.transform, glyph(x), icon_ink);
                x += icon + gap;
            }
            if let Some(label) = self.label {
                let style = self.style(theme);
                let room = rect.x1
                    - Self::pad(theme)
                    - x
                    - if self.trailing.is_some() {
                        icon + gap
                    } else {
                        0.0
                    };
                let shown = ctx.text.elide_to_width_style(
                    label,
                    crate::num::f32_of_f64(room.max(0.0)),
                    &style,
                );
                let (w, h) = ctx.text.measure_style(&shown, None, &style);
                ctx.text.draw_style(
                    ctx.scene,
                    &shown,
                    label_ink,
                    ctx.transform
                        .pre_translate((x, cy - f64::from(h) / 2.0).into()),
                    None,
                    &style,
                );
                x += f64::from(w) + gap;
            }
            if let Some(i) = self.trailing {
                i.draw(ctx.scene, ctx.transform, glyph(x), icon_ink);
            }
        }
        if self.focused {
            crate::widget::paint_focus_ring(ctx.scene, ctx.transform, rect, radius, theme);
        }
    }

    /// The accessible node of the control at `rect`.
    #[must_use]
    pub fn accessibility(&self, rect: Rect) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Button)
            .with_name(self.name)
            .with_bounds(rect)
            .with_state(AccessState::new().focusable(true).focused(self.focused))
    }

    /// Where a control of `width` stands with its leading edge at `x` on a
    /// bar whose middle line is `cy`.
    #[must_use]
    pub fn place(x: f64, cy: f64, width: f64, theme: &Theme) -> Rect {
        let h = Self::height(theme);
        Rect::new(x, cy - h / 2.0, x + width, cy + h / 2.0)
    }
}

/// The square icon button. Each constructor requires the accessible name,
/// which is also the tooltip; there is no way to make an unnamed one.
pub struct IconButton;

impl IconButton {
    /// A default (32) icon button: a quiet [`Button`] showing `icon`, named
    /// `name`.
    #[must_use]
    #[track_caller]
    pub fn new(id: &str, icon: Icon, name: &str) -> Button {
        Button::icon_only(id, icon, name)
    }

    /// A small (28) icon button, for bars and window controls.
    #[must_use]
    #[track_caller]
    pub fn small(id: &str, icon: Icon, name: &str) -> Button {
        Button::icon_only(id, icon, name).control_size(ControlSize::Small)
    }

    /// The immediate face of an icon button at `rect`, for a painted host.
    #[must_use]
    pub fn face<'a>(rect: Rect, icon: Icon, name: &'a str) -> crate::widget::ButtonFace<'a> {
        crate::widget::ButtonFace::icon_only(rect, icon, name).role(ButtonRole::Plain)
    }

    /// The square an icon button of `size` takes with its top-left at
    /// `origin`.
    #[must_use]
    pub fn square(origin: Point, size: ControlSize, theme: &Theme) -> Rect {
        let side = size.height(theme);
        Rect::new(origin.x, origin.y, origin.x + side, origin.y + side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_control_is_plain_at_rest_and_lifts_on_hover_and_while_open() {
        let theme = Theme::dark();
        let ground = theme.tokens.colors.surfaces.base;
        let face = BarControlFace::new("Workspace").label("Studio");
        assert_eq!(face.fill(&theme, ground), None, "plain at rest");
        let hover = face.hovered(true).fill(&theme, ground);
        let open = face.open(true).fill(&theme, ground);
        assert!(hover.is_some() && open.is_some());
        let step = |c: Option<Color>| crate::chrome::lightness_step(c.unwrap_or(ground), ground);
        assert!(step(open) > step(hover), "open holds the whole lift");
        assert!((BarControlFace::height(&theme) - 28.0).abs() < 1e-6);
    }

    #[test]
    fn an_icon_bar_control_is_square_and_a_labelled_one_hugs_its_label() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let icon = BarControlFace::new("Quick settings").icon(Icon::Settings);
        assert!((icon.width(&mut text, &theme) - 28.0).abs() < 1e-6);
        let short = BarControlFace::new("Workspace").label("Studio");
        let long = BarControlFace::new("Workspace").label("Studio for collaborators");
        assert!(long.width(&mut text, &theme) > short.width(&mut text, &theme));
        let chevron = short.trailing(Icon::ChevronDown);
        assert!(chevron.width(&mut text, &theme) > short.width(&mut text, &theme));
    }

    #[test]
    fn an_icon_button_carries_its_name_as_tooltip_and_accessible_name() {
        use crate::widget::Widget;
        let button = IconButton::small("close", Icon::X, "Close window");
        let node = button.accessibility().expect("a button is accessible");
        assert_eq!(node.name.as_deref(), Some("Close window"));
    }
}
