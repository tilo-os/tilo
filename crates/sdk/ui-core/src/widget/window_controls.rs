//! The window's own controls at the trailing end of its bar: maximize or
//! restore, and close.
//!
//! They belong to the window, not to a tab: they stand in the same place
//! whether the window shows one tab or several, and act on the active tab.
//! They are the one thing in the bar that is not the app's — the app's
//! actions (its search, its History, the slots it publishes) stand on the
//! window's ground and look like the app; these stand in a pill of the
//! floating material over it, so the bar says which controls belong to the
//! window and which to what the window shows.
//!
//! A Tilo window has no minimized state — it is a tab in a group, and its
//! group is what a person puts away — so the pill offers no minimize there.
//! A window of the app's own on a desktop that leaves decorations to it does
//! have one ([`WindowControls::lay_out_own_window`]).

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

/// Side of each control's square: a bar control.
pub const CONTROL_PX: f64 = crate::tokens::Metrics::BASE.bar_control_height as f64;
/// The pill's own padding: between its edge and the control faces, and
/// between two faces. It is also how far a hover ring stands off the
/// pill's edge, so the ring stays concentric with the capsule it sits in.
pub const PILL_PAD: f64 = crate::tokens::Spacing::BASE.s1 as f64;
/// Between two of the bar's other controls: the app's slots
/// ([`crate::window_toolbar`]), the new-tab button, and the room the
/// window keeps for the app's search control.
pub const CONTROL_GAP: f64 = crate::tokens::Spacing::BASE.s1 as f64;
/// Between the pill and the window's trailing edge.
pub const EDGE_INSET: f64 = crate::tokens::Spacing::BASE.s2 as f64;
/// Between the pill and whatever stands before it in the row: the window's
/// controls are a section of their own, apart from the app's by the gap
/// between sections and by the glass the app's ground has no part of, never
/// by a drawn line. One step below the gap between groups, so the app's
/// content keeps the room it names its place with in a narrow window.
pub const LEAD_GAP: f64 = crate::tokens::Spacing::BASE.rhythm.section as f64;
/// Between the app's own slots and the History toggle after them: the gap
/// two of the app's controls keep, so History reads as one more of the
/// app's actions rather than the first of the window's.
pub const HISTORY_GAP: f64 = CONTROL_GAP;
/// The box the maximize/restore glyph is drawn in: a small icon.
const ACTION_GLYPH_PX: f64 = crate::tokens::Metrics::BASE.icon_sm as f64;
/// The box the close glyph is drawn in. Lucide's × spans the middle 12 of
/// its 24 units while restore spans 18, so drawn in the same box the ×
/// would read lighter than the glyph beside it.
const CLOSE_GLYPH_PX: f64 = ACTION_GLYPH_PX * 1.5;
/// How strong the hover ring over the pill's ground is: white over a dark
/// pill, black over a light one ([`crate::tokens::Colors::hairline_on`]).
const HOVER_TINT_ALPHA: u8 = 64;
/// Close is the control that ends the window, so its ring is the danger
/// fill and its glyph white — the same pair the tabs' own × wears.
const CLOSE_RING_ALPHA: f32 = 0.8;

/// Where each control sits, and the pill behind them; `Rect::ZERO` for one
/// the window doesn't offer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowControls {
    /// The glass pill around every control: the window's own, not a
    /// control's, so it never changes with hover, press or focus.
    pub pill: Rect,
    /// The window of the app's own, on a desktop that leaves its frame to the
    /// app, is the one control a Tilo window has no state for.
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
}

/// What the controls show this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlsState {
    pub hovered: Option<WindowControl>,
    pub pressed: Option<WindowControl>,
    pub resize: Resize,
    /// How far each control's hover ring has come in, by control
    /// ([`WindowControl::index`]): on the toolkit's spring, so a ring
    /// fades in and out rather than appearing and vanishing.
    pub lifts: [f32; 3],
}

/// What the maximize control does for where the window stands now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resize {
    /// The window takes the whole strip.
    #[default]
    Maximize,
    /// The maximized window takes its width back.
    Restore,
    /// The window floating over the desk goes back into its strip, where it stood.
    Snap,
}

impl Resize {
    /// What the control does for a window that is `maximized` or `floating`: a floating window is
    /// never maximized, it snaps back.
    #[must_use]
    pub fn of(maximized: bool, floating: bool) -> Self {
        if floating {
            Resize::Snap
        } else if maximized {
            Resize::Restore
        } else {
            Resize::Maximize
        }
    }
}

impl WindowControl {
    /// Its slot in [`ControlsState::lifts`].
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            WindowControl::Maximize => 0,
            WindowControl::Close => 1,
            WindowControl::Minimize => 2,
        }
    }

    /// The control's name in the window's locale: what a screen reader says
    /// and an agent presses it by.
    #[must_use]
    pub fn name_key(self, resize: Resize) -> &'static str {
        match (self, resize) {
            // A window floating as a card folds to its header, as every card over the desk does.
            (WindowControl::Minimize, Resize::Snap) => "host-window-collapse",
            (WindowControl::Minimize, _) => "host-window-minimize",
            (WindowControl::Maximize, Resize::Maximize) => "host-window-maximize",
            (WindowControl::Maximize, Resize::Restore) => "host-window-restore",
            (WindowControl::Maximize, Resize::Snap) => "host-window-snap",
            (WindowControl::Close, _) => "host-window-close",
        }
    }

    /// The control's glyph.
    #[must_use]
    pub fn icon(self, resize: Resize) -> icons::Icon {
        match (self, resize) {
            (WindowControl::Minimize, Resize::Snap) => icons::Icon::ChevronDown,
            (WindowControl::Minimize, _) => icons::Icon::Minus,
            (WindowControl::Maximize, Resize::Maximize) => icons::Icon::Maximize2,
            (WindowControl::Maximize, Resize::Restore) => icons::Icon::Minimize2,
            (WindowControl::Maximize, Resize::Snap) => icons::Icon::Columns2,
            (WindowControl::Close, _) => icons::Icon::X,
        }
    }

    /// The target assistive technology and automation press it by.
    #[must_use]
    pub fn target(self) -> &'static str {
        match self {
            WindowControl::Minimize => "window-control-minimize",
            WindowControl::Maximize => "window-control-maximize",
            WindowControl::Close => "window-control-close",
        }
    }

    /// The control the accessibility target `target` names.
    #[must_use]
    pub fn of_target(target: &str) -> Option<Self> {
        [Self::Minimize, Self::Maximize, Self::Close]
            .into_iter()
            .find(|control| control.target() == target)
    }
}

impl WindowControls {
    /// The controls at the trailing end of `row`, centred on its centre
    /// line, inside one pill.
    #[must_use]
    pub fn lay_out(row: Rect) -> Self {
        Self::place(row, false)
    }

    /// The controls of a window the app draws the frame of itself (a desktop
    /// that leaves decorations to the client): minimize joins maximize and
    /// close, in the order desktops put them.
    #[must_use]
    pub fn lay_out_own_window(row: Rect) -> Self {
        Self::place(row, true)
    }

    /// The controls of a window floating as a card: collapse, snap back and close, standing flat
    /// on the card's own ground as a card's header controls do, with no pill of their own.
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

    /// The controls of a window this person may not close (someone else's,
    /// or one they only watch): no close, and the pill closes around the
    /// one control left.
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

    /// The pill around any window's own controls, laid out on `bar`: as tall as a bar control
    /// with the pill's padding, centred on the bar's centre line, from `controls`' leading
    /// edge less the padding to its trailing edge less the window's own inset. `Rect::ZERO`
    /// for no controls.
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

    /// Where the controls begin, less the gap anything before them keeps;
    /// the row's trailing edge for none.
    #[must_use]
    pub fn content_limit(&self, row: Rect) -> f64 {
        if self.pill.area() > 0.0 {
            self.pill.x0 - LEAD_GAP
        } else {
            row.x1
        }
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

    /// Whether `p` is on the pill at all: the window's own chrome, covering
    /// the padding around the controls.
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

    /// Paint the pill and its controls.
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

/// Paint the floating pill the window's own controls stand in: the panel ground of the
/// floating material, at the selector's radius, over the window's own ground or over the app's
/// header row. Every window's controls stand in this one pill, so a bar says the same thing
/// about them wherever the frame puts them.
pub fn paint_pill(scene: &mut Scene, transform: Affine, pill: Rect, theme: &Theme) {
    if pill.area() <= 0.0 {
        return;
    }
    let radius = crate::chrome::ChromeRole::Selector.radius(theme, pill.height());
    crate::surface::panel_unsnapped(scene, transform, pill, radius, theme);
}

/// `alpha` of an opaque line, `progress` of the way in.
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

/// How one icon button of the window's bar shows this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonFace {
    pub hovered: bool,
    pub pressed: bool,
    pub on: bool,
    /// It can act now.
    pub enabled: bool,
    /// It ends, removes or discards something: the destructive role's face.
    pub destructive: bool,
    /// It is the app's one main action: the primary role's face, the glyph on the accent, so
    /// the bar still says what the window is for where its words do not fit.
    pub primary: bool,
}

/// Paint one icon button of a window's bar with the shared button face
/// ([`crate::widget::Button::paint`]): the app's slots
/// ([`crate::window_toolbar`]), the History toggle and the new-tab button
/// are the same control, so everything the app provides in the bar stands on
/// the window's ground as one row of them — the window's own controls are
/// the pill's, not these.
pub fn paint_icon_button(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    (rect, icon): (Rect, icons::Icon),
    face: ButtonFace,
) {
    let role = if face.destructive {
        crate::widget::ButtonRole::Destructive
    } else if face.primary {
        crate::widget::ButtonRole::Primary
    } else {
        crate::widget::ButtonRole::Plain
    };
    let face = crate::widget::ButtonFace::icon_only(rect, icon, "")
        .role(role)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(face.hovered && face.enabled)
        .pressed(face.pressed && face.enabled)
        .toggled(face.on)
        .disabled(!face.enabled);
    crate::widget::Button::paint(scene, transform, text, theme, &face);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Rect {
        Rect::new(0.0, 0.0, 800.0, 40.0)
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
    fn a_row_with_no_room_has_no_controls() {
        let c = WindowControls::lay_out(Rect::ZERO);
        assert_eq!(c, WindowControls::default());
        assert_eq!(c.iter().count(), 0);
        assert_eq!(c.content_limit(row()), row().x1);
    }
}
