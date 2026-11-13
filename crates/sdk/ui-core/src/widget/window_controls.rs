//! The window's own controls at the trailing end of its bar: maximize or restore, and close.
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use crate::icons;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowControl {
    Minimize,
    Maximize,
    Close,
}

pub const CONTROL_PX: f64 = crate::tokens::Metrics::BASE.bar_control_height as f64;
pub const PILL_PAD: f64 = crate::tokens::Spacing::BASE.s1 as f64;
pub const CONTROL_GAP: f64 = crate::tokens::Spacing::BASE.s1 as f64;
pub const EDGE_INSET: f64 = crate::tokens::Spacing::BASE.s2 as f64;
/// Between the pill and whatever stands before it in the row: the window's
/// controls are a section of their own, apart from the app's by the gap
/// between sections and by the glass the app's ground has no part of, never
/// by a drawn line. One step below the gap between groups, so the app's
/// content keeps the room it names its place with in a narrow window.
pub const LEAD_GAP: f64 = crate::tokens::Spacing::BASE.rhythm.section as f64;
pub const HISTORY_GAP: f64 = CONTROL_GAP;
const ACTION_GLYPH_PX: f64 = crate::tokens::Metrics::BASE.icon_sm as f64;
const CLOSE_GLYPH_PX: f64 = ACTION_GLYPH_PX * 1.5;
const HOVER_TINT_ALPHA: u8 = 64;
const CLOSE_RING_ALPHA: f32 = 0.8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowControls {
    /// The glass pill around every control: the window's own, not a
    /// control's, so it never changes with hover, press or focus.
    pub pill: Rect,
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlsState {
    pub hovered: Option<WindowControl>,
    pub pressed: Option<WindowControl>,
    pub resize: Resize,
    pub lifts: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resize {
    #[default]
    Maximize,
    Restore,
    Snap,
    Expand,
}

impl Resize {
    /// What the control does for a window that is `maximized`, `floating` or folded to its header
    /// as a card (`collapsed`): a floating window is never maximized.
    #[must_use]
    pub fn of_window(maximized: bool, floating: bool, collapsed: bool) -> Self {
        match (floating, collapsed) {
            (true, true) => Resize::Expand,
            (true, false) => Resize::Snap,
            (false, _) => {
                if maximized {
                    Resize::Restore
                } else {
                    Resize::Maximize
                }
            }
        }
    }

    /// What the control does for a window that is `maximized` or `floating`: a floating window is
    /// never maximized, it snaps back.
    #[must_use]
    pub fn of(maximized: bool, floating: bool) -> Self {
        Self::of_window(maximized, floating, false)
    }
}

impl WindowControl {
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            WindowControl::Maximize => 0,
            WindowControl::Close => 1,
            WindowControl::Minimize => 2,
        }
    }

    #[must_use]
    pub fn name_key(self, resize: Resize) -> &'static str {
        match (self, resize) {
            (WindowControl::Minimize, Resize::Snap) => "host-window-collapse",
            (WindowControl::Minimize, Resize::Expand) => "host-window-expand",
            (WindowControl::Minimize, _) => "host-window-minimize",
            (WindowControl::Maximize, Resize::Maximize) => "host-window-maximize",
            (WindowControl::Maximize, Resize::Restore) => "host-window-restore",
            (WindowControl::Maximize, Resize::Snap | Resize::Expand) => "host-window-snap",
            (WindowControl::Close, _) => "host-window-close",
        }
    }

    #[must_use]
    pub fn icon(self, resize: Resize) -> icons::Icon {
        match (self, resize) {
            (WindowControl::Minimize, Resize::Snap) => icons::Icon::ChevronDown,
            (WindowControl::Minimize, Resize::Expand) => icons::Icon::ChevronUp,
            (WindowControl::Minimize, _) => icons::Icon::Minus,
            (WindowControl::Maximize, Resize::Maximize) => icons::Icon::Maximize2,
            (WindowControl::Maximize, Resize::Restore) => icons::Icon::Minimize2,
            (WindowControl::Maximize, Resize::Snap | Resize::Expand) => icons::Icon::Columns2,
            (WindowControl::Close, _) => icons::Icon::X,
        }
    }

    #[must_use]
    pub fn target(self) -> &'static str {
        match self {
            WindowControl::Minimize => "window-control-minimize",
            WindowControl::Maximize => "window-control-maximize",
            WindowControl::Close => "window-control-close",
        }
    }

    #[must_use]
    pub fn of_target(target: &str) -> Option<Self> {
        [Self::Minimize, Self::Maximize, Self::Close]
            .into_iter()
            .find(|control| control.target() == target)
    }
}

impl WindowControls {
    #[must_use]
    pub fn lay_out(row: Rect) -> Self {
        Self::place(row, false)
    }

    #[must_use]
    pub fn lay_out_own_window(row: Rect) -> Self {
        Self::place(row, true)
    }

    #[must_use]
    pub fn lay_out_card(row: Rect) -> Self {
        Self {
            pill: Rect::ZERO,
            ..Self::place(row, true)
        }
    }

    fn place(row: Rect, minimize: bool) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        let pill_h = (CONTROL_PX + 2.0 * PILL_PAD).min(row.height());
        let pad = (pill_h - CONTROL_PX) / 2.0;
        let y0 = row.center().y - pill_h / 2.0;
        let face_y0 = y0 + (pill_h - CONTROL_PX) / 2.0;
        let square = |x1: f64| Rect::new(x1 - CONTROL_PX, face_y0, x1, face_y0 + CONTROL_PX);
        let pill_x1 = row.x1 - EDGE_INSET;
        let close = square(pill_x1 - pad);
        let maximize = square(close.x0 - PILL_PAD);
        let minimize = if minimize {
            let minimum = square(maximize.x0 - PILL_PAD);
            if minimum.x0 < row.x0 {
                return Self::default();
            }
            minimum
        } else {
            Rect::ZERO
        };
        let lead = if minimize.area() > 0.0 {
            minimize.x0
        } else {
            maximize.x0
        };
        let pill = Rect::new(lead - pad, y0, pill_x1.max(maximize.x1 + pad), y0 + pill_h);
        Self {
            pill,
            minimize,
            maximize,
            close,
        }
    }

    #[must_use]
    pub fn without_close(self) -> Self {
        if self.close.area() <= 0.0 {
            return self;
        }
        if self.maximize.area() <= 0.0 {
            return Self::default();
        }
        let maximize =
            self.maximize + vello::kurbo::Vec2::new(self.close.x1 - self.maximize.x1, 0.0);
        let pad = (self.pill.height() - CONTROL_PX) / 2.0;
        Self {
            pill: Rect::new(
                maximize.x0 - pad,
                self.pill.y0,
                maximize.x1 + pad,
                self.pill.y1,
            ),
            minimize: Rect::ZERO,
            maximize,
            close: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn around(bar: Rect, controls: Rect) -> Rect {
        if controls.area() <= 0.0 || bar.height() <= 0.0 || controls.width() <= 0.0 {
            return Rect::ZERO;
        }
        let height = (CONTROL_PX + 2.0 * PILL_PAD).min(bar.height());
        let x0 = controls.x0 - PILL_PAD;
        let x1 = (controls.x1 + PILL_PAD).min(bar.x1 - EDGE_INSET);
        Rect::new(
            x0,
            bar.center().y - height / 2.0,
            x1.max(x0 + height),
            bar.center().y + height / 2.0,
        )
    }

    #[must_use]
    pub fn content_limit(&self, row: Rect) -> f64 {
        let lead = if self.pill.area() > 0.0 {
            self.pill.x0
        } else {
            self.iter()
                .map(|(_, at)| at.x0)
                .fold(row.x1 + LEAD_GAP, f64::min)
        };
        lead - LEAD_GAP
    }

    pub fn iter(&self) -> impl Iterator<Item = (WindowControl, Rect)> {
        [
            (WindowControl::Minimize, self.minimize),
            (WindowControl::Maximize, self.maximize),
            (WindowControl::Close, self.close),
        ]
        .into_iter()
        .filter(|(_, r)| r.area() > 0.0)
    }

    #[must_use]
    pub fn hit(&self, p: Point) -> Option<WindowControl> {
        self.iter().find(|(_, r)| r.contains(p)).map(|(c, _)| c)
    }

    #[must_use]
    pub fn on_pill(&self, p: Point) -> bool {
        self.pill.area() > 0.0 && self.pill.contains(p)
    }

    pub fn offset(&mut self, shift: vello::kurbo::Vec2) {
        for r in [
            &mut self.pill,
            &mut self.minimize,
            &mut self.maximize,
            &mut self.close,
        ] {
            if r.area() > 0.0 {
                *r = *r + shift;
            }
        }
    }

    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme, state: ControlsState) {
        if self.iter().next().is_none() {
            return;
        }
        paint_pill(scene, transform, self.pill, theme);
        let ground = crate::chrome::panel_ground(theme);
        // The pill is the ground its controls stand on: their ink and their
        // hover ring are resolved against it, never against the window's.
        let theme = &theme.on_ground(ground);
        for (control, rect) in self.iter() {
            let hovered = state.hovered == Some(control);
            let pressed = state.pressed == Some(control);
            let lift = f64::from(
                state
                    .lifts
                    .get(control.index())
                    .copied()
                    .unwrap_or_default()
                    .clamp(0.0, 1.0),
            );
            // Lit at once under a press; otherwise as far in as the spring
            // has brought it — coming in, or going out from where it was.
            let ring = if pressed { 1.0 } else { lift };
            let glyph = control.icon(state.resize);
            let glyph_box = match control {
                WindowControl::Close => square_at(rect.center(), CLOSE_GLYPH_PX),
                WindowControl::Minimize | WindowControl::Maximize => {
                    square_at(rect.center(), ACTION_GLYPH_PX)
                }
            };
            let colour = match control {
                WindowControl::Close => theme
                    .tokens
                    .colors
                    .semantic
                    .danger
                    .with_alpha(CLOSE_RING_ALPHA * crate::num::f32_of_f64(ring)),
                WindowControl::Minimize | WindowControl::Maximize => {
                    crate::tokens::Colors::hairline_on(ground, scale_alpha(HOVER_TINT_ALPHA, ring))
                }
            };
            if ring > 0.0 {
                // A press presses the face in, as every shared control's
                // does: the theme's own response scale, never a number here.
                let scale = f64::from(theme.tokens.response.step(hovered, pressed).scale);
                let face_rect = Rect::from_center_size(rect.center(), rect.size() * scale);
                let face = RoundedRect::from_rect(face_rect, face_rect.height() / 2.0);
                scene.fill(Fill::NonZero, transform, colour, None, &face);
            }
            // The glyph reads on whatever it stands on: the pill's ground at
            // rest, the face lit under the pointer — never a colour picked
            // here.
            let ink = theme.tokens.colors.ink_on_any(
                &[ground, crate::tokens::colors::composite(colour, ground)],
                theme.tokens.colors.text_primary,
                crate::tokens::colors::ON_FILL_MIN_CONTRAST,
            );
            icons::draw_icon(
                scene,
                transform,
                glyph_box,
                glyph,
                ink,
                icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }
}

pub fn paint_pill(scene: &mut Scene, transform: Affine, pill: Rect, theme: &Theme) {
    if pill.area() <= 0.0 {
        return;
    }
    let radius = crate::chrome::ChromeRole::Selector.radius(theme, pill.height());
    crate::surface::panel_unsnapped(scene, transform, pill, radius, theme);
}

fn scale_alpha(alpha: u8, progress: f64) -> u8 {
    let scaled = f64::from(alpha) * progress.clamp(0.0, 1.0);
    crate::num::u8_of_f64(scaled)
}

/// A `side`-px square centred on `c`.
fn square_at(c: Point, side: f64) -> Rect {
    Rect::new(
        c.x - side / 2.0,
        c.y - side / 2.0,
        c.x + side / 2.0,
        c.y + side / 2.0,
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonFace {
    pub hovered: bool,
    pub pressed: bool,
    pub on: bool,
    pub enabled: bool,
    pub destructive: bool,
    /// It is the app's one main action: the primary role's face, the glyph on the accent, so
    /// the bar still says what the window is for where its words do not fit.
    pub primary: bool,
}

pub fn paint_icon_button(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    (rect, icon): (Rect, icons::Icon),
    face: ButtonFace,
) {
    crate::widget::Button::paint(
        scene,
        transform,
        text,
        theme,
        &icon_button_face(rect, icon, face),
    );
}

#[must_use]
pub fn icon_button_face(
    rect: Rect,
    icon: icons::Icon,
    face: ButtonFace,
) -> crate::widget::ButtonFace<'static> {
    let role = if face.destructive {
        crate::widget::ButtonRole::Destructive
    } else if face.primary {
        crate::widget::ButtonRole::Primary
    } else {
        crate::widget::ButtonRole::Plain
    };
    crate::widget::ButtonFace::icon_only(rect, icon, "")
        .role(role)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(face.hovered && face.enabled)
        .pressed(face.pressed && face.enabled)
        .toggled(face.on)
        .disabled(!face.enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Rect {
        Rect::new(0.0, 0.0, 800.0, 40.0)
    }

    #[test]
    fn a_folded_cards_collapse_control_points_up() {
        assert_eq!(Resize::of_window(false, true, false), Resize::Snap);
        assert_eq!(Resize::of_window(false, true, true), Resize::Expand);
        assert_eq!(
            WindowControl::Minimize.icon(Resize::Snap),
            icons::Icon::ChevronDown
        );
        assert_eq!(
            WindowControl::Minimize.icon(Resize::Expand),
            icons::Icon::ChevronUp
        );
        assert_eq!(
            WindowControl::Minimize.name_key(Resize::Expand),
            "host-window-expand"
        );
        assert_eq!(
            WindowControl::Minimize.name_key(Resize::Snap),
            "host-window-collapse"
        );
    }

    #[test]
    fn close_is_outermost_and_both_controls_share_one_pill() {
        let c = WindowControls::lay_out(row());
        assert!(c.close.x1 < row().x1 && c.close.x1 > row().x1 - 2.0 * EDGE_INSET);
        assert!(c.maximize.x1 <= c.close.x0, "maximize stands before close");
        assert_eq!(c.iter().count(), 2);
        assert!(
            c.pill.contains(c.maximize.center()) && c.pill.contains(c.close.center()),
            "both controls stand inside the one pill",
        );
        assert_eq!(c.pill.x1, row().x1 - EDGE_INSET);
        assert!(
            (c.pill.height() - (CONTROL_PX + 2.0 * PILL_PAD)).abs() < f64::EPSILON,
            "the pill is a bar control tall on each side",
        );
        assert!(
            (c.pill.center().y - row().center().y).abs() < f64::EPSILON,
            "the pill is centred on the tab row",
        );
        assert!(
            (c.pill.width() - (2.0 * CONTROL_PX + 3.0 * PILL_PAD)).abs() < f64::EPSILON,
            "two faces and the padding around and between them",
        );
    }

    #[test]
    fn a_window_that_is_not_ones_to_close_keeps_one_control_in_a_round_pill() {
        let c = WindowControls::lay_out(row());
        let held = c.without_close();
        assert_eq!(held.iter().count(), 1);
        assert_eq!(held.close, Rect::ZERO);
        assert_eq!(
            held.maximize, c.close,
            "the outermost control stays outermost"
        );
        assert!(
            (held.pill.width() - held.pill.height()).abs() < f64::EPSILON,
            "one control's pill is a round pill",
        );
        assert!(held.content_limit(row()) > c.content_limit(row()));
        assert_eq!(held.hit(c.close.center()), Some(WindowControl::Maximize));
    }

    #[test]
    fn content_stops_before_the_controls() {
        let c = WindowControls::lay_out(row());
        assert!(c.content_limit(row()) <= c.pill.x0 - LEAD_GAP + f64::EPSILON);
        assert_eq!(c.hit(c.close.center()), Some(WindowControl::Close));
        assert_eq!(c.hit(c.maximize.center()), Some(WindowControl::Maximize));
        assert_eq!(c.hit(Point::new(10.0, 22.0)), None);
        assert!(c.on_pill(Point::new(c.pill.x0 + 1.0, c.pill.center().y)));
        assert!(!c.on_pill(Point::new(10.0, 22.0)));
    }

    #[test]
    fn the_window_s_controls_stand_in_one_pill_inside_its_bar() {
        let bar = Rect::new(0.0, 0.0, 800.0, crate::widget::tabs::TAB_BAR_HEIGHT);
        let controls = Rect::new(700.0, 8.0, 788.0, 36.0);
        let pill = WindowControls::around(bar, controls);
        assert!(
            pill.contains(controls.center()) && pill.area() > controls.area(),
            "the pill holds the controls with room around them: {pill:?} for {controls:?}"
        );
        assert!(pill.height() <= bar.height() + f64::EPSILON);
        assert!(
            (pill.x1 - (bar.x1 - EDGE_INSET)).abs() < f64::EPSILON,
            "the pill stands the window's own inset from its trailing edge: {pill:?}"
        );
        assert!(
            (pill.center().y - bar.center().y).abs() < f64::EPSILON,
            "the pill is centred on the bar",
        );
        assert_eq!(WindowControls::around(bar, Rect::ZERO), Rect::ZERO);
    }

    #[test]
    fn a_cards_title_stops_before_its_controls() {
        let c = WindowControls::lay_out_card(row());
        assert_eq!(c.pill, Rect::ZERO, "a card's controls stand with no pill");
        assert!(
            c.content_limit(row()) <= c.minimize.x0 - LEAD_GAP + f64::EPSILON,
            "the title ellipsizes before the collapse, snap and close: {c:?}"
        );
    }

    #[test]
    fn a_row_with_no_room_has_no_controls() {
        let c = WindowControls::lay_out(Rect::ZERO);
        assert_eq!(c, WindowControls::default());
        assert_eq!(c.iter().count(), 0);
        assert_eq!(c.content_limit(row()), row().x1);
    }
}
