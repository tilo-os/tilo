//! `BarControl` and `IconButton` (`docs/design-language.md`, "Primitives").

use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::widget::{Button, ButtonRole, ControlSize};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};

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

    #[must_use]
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = (!label.is_empty()).then_some(label);
        self
    }

    #[must_use]
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
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

    /// Keyboard focus. The ring shows only while focus is visible
    /// ([`tilo_ui_base::focus_visible`]).
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

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
    /// shell for the top bar, the window for a window bar).
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
    /// height [`Self::height`]) on `ground`. Inks are measured on
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
                let spare = rect.x1
                    - Self::pad(theme)
                    - x
                    - if self.trailing.is_some() {
                        icon + gap
                    } else {
                        0.0
                    };
                let shown = ctx.text.elide_to_width_style(
                    label,
                    tilo_ui_base::num::f32_of_f64(spare.max(0.0)),
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

    /// Paint only the control's lift in `rect` on `ground`: the fill [`Self::paint`] lays under
    /// its content, for a bar control that draws its content itself (an application's tray
    /// icon, a mark).
    pub fn paint_lift(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        rect: Rect,
        theme: &Theme,
        ground: Color,
    ) {
        let Some(fill) = self.fill(theme, ground) else {
            return;
        };
        let radius = f64::from(theme.tokens.radii.control).min(rect.height() / 2.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            fill,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }

    pub fn accessibility(&self, rect: Rect) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
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
    pub fn regular(id: &str, icon: Icon, name: &str) -> Button {
        Button::icon_only(id, icon, name)
    }

    /// A small (28) icon button, for bars and window controls.
    #[must_use]
    #[track_caller]
    pub fn small(id: &str, icon: Icon, name: &str) -> Button {
        Button::icon_only(id, icon, name).control_size(ControlSize::Small)
    }

    #[must_use]
    pub fn face(rect: Rect, icon: Icon, name: &str) -> crate::widget::ButtonFace<'_> {
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

    tilo_test_env::scenario! {
        fn a_bar_control_is_plain_square_or_labelled_and_named;



        phase an_icon_button_carries_its_name_as_tooltip_and_accessible_name;
        {
            use tilo_ui_base::widget::Widget;
            let button = IconButton::small("close", Icon::X, "Close window");
            let node = button.accessibility().expect("a button is accessible");
            assert_eq!(node.name.as_deref(), Some("Close window"));
        }
    }
}
