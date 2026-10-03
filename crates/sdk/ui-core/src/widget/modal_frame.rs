//! The one modal chrome: scrim, card, title row, close control, action row.
//! Retained [`Widget`](crate::widget::Widget)s ([`ModalDialog`](super::modal_dialog::ModalDialog)) and
//! immediate-mode painters share this implementation, including the focusable,
//! accessible controls.
//!
//! The action row goes through [`Button::paint`], so a modal button *is* a
//! `Button`.
//!
//! The body sits between the two paint calls, because the card must be
//! under it while the action row is laid out bottom-up:
//!
//! ```ignore
//! let layout = ModalFrame::layout(viewport, &spec, text_renderer, theme);
//! ModalFrame::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
//! // ... caller draws its own content inside `layout.body` ...
//! ModalFrame::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
//! ```

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::icons::Icon;
use crate::platform::{DialogActionOrder, PlatformConventions};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::button::{Button, ButtonFace, ButtonRole};

/// Strength of the scrim painted over everything behind the modal. 0.45
/// rather than the 0.30 a design tool would specify: vello has no backdrop
/// blur, so an un-blurred 0.30 lets through enough structure to read as
/// floating over noise; 0.45 reads like a blurred 0.30.
pub const MODAL_SCRIM_ALPHA: f32 = 0.45;

/// Widest a modal card is drawn. Past this a single column of text stops being
/// one column and starts being a paragraph nobody tracks across.
pub const MODAL_MAX_WIDTH: f64 = 520.0;
/// Narrowest a modal card is drawn before it starts eliding instead.
pub const MODAL_MIN_WIDTH: f64 = 320.0;
/// Gap kept between the card and every viewport edge, so the card never
/// reaches an edge even on a small output.
pub const MODAL_VIEWPORT_MARGIN: f64 = 24.0;
/// Height of the title row — title plus the close control's hit target.
pub const MODAL_TITLE_ROW_H: f64 = 52.0;
/// Height of the action row, including the padding above and below it.
pub const MODAL_ACTION_ROW_H: f64 = 56.0;
/// Padding from the card edge to its content, on every side.
pub const MODAL_PAD: f64 = 20.0;
/// Side of the square close-control hit target: 14 px is the size an `X`
/// wants to be, 32 is the size a thing you must hit wants to be.
pub const MODAL_CLOSE_SIZE: f64 = 32.0;

/// Height of one action button, matching the compact chrome button height the
/// rest of the overlay surfaces use.
const ACTION_H: f64 = 34.0;
/// Gap between two action buttons.
const ACTION_GAP: f64 = 8.0;
/// Narrowest an action button is laid out, so `OK` and `Copy` are still
/// buttons rather than badges.
const ACTION_MIN_W: f64 = 88.0;
/// Horizontal padding added around an action's measured label.
const ACTION_PAD_X: f64 = 28.0;
/// Contrast a modal's own ink must clear against the card before it is used
/// as-is. WCAG 2.1 AA for body text.
const MODAL_MIN_INK_CONTRAST: f64 = 4.5;

/// How an action is weighted in the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalEmphasis {
    /// The default action. One per modal, accent-filled, and the one Enter
    /// fires.
    Primary,
    /// Everything else — Cancel, and any additional verb.
    Secondary,
    /// A primary action that destroys something. Danger-filled, and still what
    /// Enter would fire.
    Danger,
}

/// One button in the action row.
// Four independent interaction axes (enabled, hovered, pressed, focused)
// that can co-occur in any combination — not a state machine an enum
// would clarify.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent interaction-state flags, not related toggles"
)]
#[derive(Clone, Copy, Debug)]
pub struct ModalAction<'a> {
    pub label: &'a str,
    pub emphasis: ModalEmphasis,
    /// A disabled action is painted dimmed and is not hit-testable.
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
    /// Keyboard focus is on this action, so it draws a focus ring and is what
    /// Enter/Space activates.
    pub focused: bool,
}

impl<'a> ModalAction<'a> {
    pub fn primary(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Primary)
    }
    pub fn secondary(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Secondary)
    }
    pub fn danger(label: &'a str) -> Self {
        Self::of(label, ModalEmphasis::Danger)
    }
    fn of(label: &'a str, emphasis: ModalEmphasis) -> Self {
        Self {
            label,
            emphasis,
            enabled: true,
            hovered: false,
            pressed: false,
            focused: false,
        }
    }
    #[must_use]
    pub fn enabled(mut self, v: bool) -> Self {
        self.enabled = v;
        self
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.hovered = v;
        self
    }
    #[must_use]
    pub fn pressed(mut self, v: bool) -> Self {
        self.pressed = v;
        self
    }
    /// Set it and the action draws the focus ring every other control draws. A
    /// modal whose buttons never take this is one a keyboard user cannot see
    /// their way around.
    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
}

/// What a modal is being asked to frame.
// Four independent flags (closable, the close control's own hover/focus,
// and whether the scrim dismisses) — each toggled by a different caller
// concern, not a combinatorial state an enum would simplify.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent spec flags set by unrelated callers, not related toggles"
)]
#[derive(Clone, Debug)]
pub struct ModalSpec<'a> {
    /// Shown in the title row. A modal without one is a modal whose purpose
    /// the person has to infer from its body.
    pub title: &'a str,
    /// Draw a close control. Escape always dismisses; this is the visible half
    /// of that, on by default because a surface whose only way out is a key
    /// nobody can see is the defect this widget exists to stop repeating.
    pub closable: bool,
    /// Actions in semantic authoring order. Geometry follows
    /// [`Self::platform`], so callers do not branch on native button order.
    pub actions: &'a [ModalAction<'a>],
    /// Native shortcut and dialog presentation conventions.
    pub platform: PlatformConventions,
    /// Height the caller's body content wants, in logical pixels.
    pub body_height: f64,
    /// Widest the card may be drawn. Defaults to [`MODAL_MAX_WIDTH`].
    pub max_width: f64,
    pub close_hovered: bool,
    pub close_focused: bool,
    /// Whether a scrim click dismisses. The scrim **always** swallows the
    /// click either way — a surface that lets clicks through is not modal.
    /// Turn this off for a modal that would lose work if a stray click closed
    /// it.
    pub dismiss_on_scrim: bool,
}

impl Default for ModalSpec<'_> {
    fn default() -> Self {
        Self {
            title: "",
            closable: true,
            actions: &[],
            platform: PlatformConventions::default(),
            body_height: 0.0,
            max_width: MODAL_MAX_WIDTH,
            close_hovered: false,
            close_focused: false,
            dismiss_on_scrim: true,
        }
    }
}

impl<'a> ModalSpec<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            ..Self::default()
        }
    }
    #[must_use]
    pub fn actions(mut self, actions: &'a [ModalAction<'a>]) -> Self {
        self.actions = actions;
        self
    }
    #[must_use]
    pub fn platform_conventions(mut self, platform: PlatformConventions) -> Self {
        self.platform = platform;
        self
    }
    #[must_use]
    pub fn body_height(mut self, h: f64) -> Self {
        self.body_height = h;
        self
    }
    #[must_use]
    pub fn max_width(mut self, w: f64) -> Self {
        self.max_width = w;
        self
    }
    #[must_use]
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    #[must_use]
    pub fn dismiss_on_scrim(mut self, v: bool) -> Self {
        self.dismiss_on_scrim = v;
        self
    }
    #[must_use]
    pub fn close_hovered(mut self, v: bool) -> Self {
        self.close_hovered = v;
        self
    }
    #[must_use]
    pub fn close_focused(mut self, v: bool) -> Self {
        self.close_focused = v;
        self
    }
}

/// Where every part of the modal landed. Pure geometry, computed without a
/// GPU, so the layout rules can be tested directly rather than inferred from a
/// rendered frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ModalLayout {
    /// The whole viewport. The scrim covers this.
    pub scrim: Rect,
    pub card: Rect,
    /// The band the title is drawn in — from the card's left padding to
    /// wherever the close control starts.
    pub title: Rect,
    /// The close control's hit target, when the spec asked for one.
    pub close: Option<Rect>,
    /// The region the caller owns. Content taller than this must scroll or
    /// elide inside it — see [`Self::body_overflows`].
    pub body: Rect,
    /// One rect per entry in `spec.actions`, in the same order.
    pub actions: Vec<Rect>,
    /// The caller asked for more height than a centred card can be given. The
    /// card does not grow past the viewport; the caller scrolls.
    pub body_overflows: bool,
    /// Index of the action Enter should fire, if any.
    pub primary: Option<usize>,
    /// Mirrors [`ModalSpec::dismiss_on_scrim`], so a caller holding only the
    /// layout can still answer a scrim click correctly.
    pub dismiss_on_scrim: bool,
}

/// What a point landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalHit {
    Close,
    /// An action, by index into `spec.actions`.
    Action(usize),
    /// Inside the card but not on a control. Swallow it.
    Card,
    /// The scrim. Dismiss when [`ModalLayout::dismiss_on_scrim`], otherwise
    /// swallow.
    Scrim,
}

impl ModalLayout {
    /// Resolve a point to whatever it hit, controls first so an action that
    /// overlaps the card (all of them do) resolves to the action. `None` only
    /// for a point outside the viewport entirely.
    pub fn hit(&self, p: Point) -> Option<ModalHit> {
        if let Some(c) = self.close
            && c.contains(p)
        {
            return Some(ModalHit::Close);
        }
        for (i, r) in self.actions.iter().enumerate() {
            if r.area() > 0.0 && r.contains(p) {
                return Some(ModalHit::Action(i));
            }
        }
        if self.card.contains(p) {
            return Some(ModalHit::Card);
        }
        if self.scrim.contains(p) {
            return Some(ModalHit::Scrim);
        }
        None
    }

    /// True for the close control, and for the scrim when the spec allows it.
    /// Everything else is swallowed.
    pub fn dismisses(&self, p: Point) -> bool {
        match self.hit(p) {
            Some(ModalHit::Close) => true,
            Some(ModalHit::Scrim) => self.dismiss_on_scrim,
            _ => false,
        }
    }
}

/// The modal chrome. A namespace, not a state machine: every entry point is a
/// free function over `(viewport, spec)`, so an immediate-mode caller keeps
/// owning its own state.
pub struct ModalFrame;

impl ModalFrame {
    /// The card's own fill.
    pub fn card_fill(theme: &Theme) -> Color {
        // Panel role (HIG §9.1): the panel ground, walked clear of both the
        // desktop and the window card, so the sheet separates by value as
        // well as by edge on every preset.
        crate::chrome::panel_ground(theme)
    }

    /// Ink that is legible on the modal card, as `(primary, dim)`.
    ///
    /// The chrome's own `text_primary` when it clears AA against the card,
    /// else the contrast-guaranteed on-card ramp — a fixed ramp slot can read
    /// on one palette and vanish on another.
    pub fn ink(theme: &Theme) -> (Color, Color) {
        let card = Self::card_fill(theme);
        if crate::a11y::contrast_ratio(theme.text_primary, card) >= MODAL_MIN_INK_CONTRAST {
            (theme.text_primary, theme.text_dim)
        } else {
            let on = theme.on_content();
            (on.ink, on.ink_dim)
        }
    }

    /// The factor the frame's own chrome — padding, viewport margin, title
    /// and action rows, the close target — scales by. Read in exactly one
    /// place so the rule can change once.
    ///
    /// Today this is `text_scale`. The spacing/metrics rule says chrome
    /// follows `ui_scale` only (`Theme::with_ui_scale`), but
    /// `identity_flow::sheet` hands its measured body heights back divided
    /// by `text_scale` and predicts the body width with the same formula, so
    /// flipping this alone would mis-size every identity sheet at a large
    /// text setting. Once that consumer reads [`Self::body_width`] and
    /// [`Self::action_height`], this becomes `f64::from(theme.ui_scale)`.
    fn chrome_scale(theme: &Theme) -> f64 {
        f64::from(theme.text_scale)
    }

    /// Width of the card for `spec` inside `viewport`, knowable before any
    /// body row is measured: it depends on the viewport and the spec's
    /// maximum, never on the body's height. Clamped to the viewport minus
    /// [`MODAL_VIEWPORT_MARGIN`] on both sides, and never narrower than
    /// [`MODAL_MIN_WIDTH`] unless the viewport itself is.
    pub fn card_width(viewport: Rect, spec: &ModalSpec<'_>, theme: &Theme) -> f64 {
        let scale = Self::chrome_scale(theme);
        let avail_w = (viewport.width() - MODAL_VIEWPORT_MARGIN * scale * 2.0).max(0.0);
        (spec.max_width * scale)
            .min(avail_w)
            .max((MODAL_MIN_WIDTH * scale).min(avail_w))
    }

    /// Width the caller's body is laid into — the card minus its padding on
    /// both sides. Equal to `layout(..).body.width()` for the same inputs,
    /// so a caller can size its rows before it knows how tall they are.
    pub fn body_width(viewport: Rect, spec: &ModalSpec<'_>, theme: &Theme) -> f64 {
        let scale = Self::chrome_scale(theme);
        (Self::card_width(viewport, spec, theme) - MODAL_PAD * scale * 2.0).max(0.0)
    }

    /// Height of one action button, in the same pixels as the rects
    /// [`Self::layout`] hands back. A body control that wants to sit on the
    /// action row's line reads this rather than restating the number.
    pub fn action_height(theme: &Theme) -> f64 {
        ACTION_H * Self::chrome_scale(theme)
    }

    /// Compute the geometry for `spec` inside `viewport`.
    pub fn layout(
        viewport: Rect,
        spec: &ModalSpec<'_>,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> ModalLayout {
        let tokens = &theme.tokens;
        let scale = Self::chrome_scale(theme);
        // `TextRenderer` applies the accessibility multiplier. Theme type
        // tokens already carry it too, so recover the design size for direct
        // renderer calls and scale the surrounding geometry exactly once.
        let body_style = tokens.typography.style(TextRole::Caption);

        let w = Self::card_width(viewport, spec, theme);
        let button_h = Self::action_height(theme);

        let action_h = if spec.actions.is_empty() {
            0.0
        } else {
            MODAL_ACTION_ROW_H * scale
        };
        let chrome_h = MODAL_TITLE_ROW_H * scale + action_h + MODAL_PAD * scale;
        let wanted_h = chrome_h + spec.body_height.max(0.0) * scale;
        let avail_h = (viewport.height() - MODAL_VIEWPORT_MARGIN * scale * 2.0).max(0.0);
        let h = wanted_h.min(avail_h);
        // The card never grows past the viewport: an action row nobody can
        // reach is worse than a body that scrolls.
        let body_overflows = wanted_h > avail_h + 0.5;

        let cx = f64::midpoint(viewport.x0, viewport.x1);
        let cy = f64::midpoint(viewport.y0, viewport.y1);
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);

        let close = if spec.closable {
            let x1 = card.x1 - MODAL_PAD * scale + 6.0 * scale;
            let mid = card.y0 + MODAL_TITLE_ROW_H * scale / 2.0;
            Some(Rect::new(
                x1 - MODAL_CLOSE_SIZE * scale,
                mid - MODAL_CLOSE_SIZE * scale / 2.0,
                x1,
                mid + MODAL_CLOSE_SIZE * scale / 2.0,
            ))
        } else {
            None
        };
        // The title stops where the close control starts, so a long title
        // elides instead of running under the X.
        let title_x1 = close.map_or(card.x1 - MODAL_PAD * scale, |c| c.x0 - 10.0 * scale);
        let title = Rect::new(
            card.x0 + MODAL_PAD * scale,
            card.y0,
            title_x1.max(card.x0 + MODAL_PAD * scale),
            card.y0 + MODAL_TITLE_ROW_H * scale,
        );

        let body = Rect::new(
            card.x0 + MODAL_PAD * scale,
            card.y0 + MODAL_TITLE_ROW_H * scale,
            card.x1 - MODAL_PAD * scale,
            (card.y1 - action_h - MODAL_PAD * scale).max(card.y0 + MODAL_TITLE_ROW_H * scale),
        );

        // Keep geometry indexed by the authored action while ordering its
        // visual positions from semantic emphasis and platform convention.
        let mut actions = vec![Rect::ZERO; spec.actions.len()];
        if !spec.actions.is_empty() {
            let by = card.y1 - MODAL_PAD * scale - button_h;
            let mut bx = card.x1 - MODAL_PAD * scale;
            let mut visual_order: Vec<usize> = (0..spec.actions.len()).collect();
            visual_order.sort_by_key(|&index| {
                let primary = spec.actions.get(index).is_some_and(|action| {
                    matches!(
                        action.emphasis,
                        ModalEmphasis::Primary | ModalEmphasis::Danger
                    )
                });
                match spec.platform.dialog_action_order() {
                    DialogActionOrder::PrimaryFirst => !primary,
                    DialogActionOrder::PrimaryLast => primary,
                }
            });
            for &index in visual_order.iter().rev() {
                let Some(a) = spec.actions.get(index) else {
                    continue;
                };
                let (tw, _) = text_renderer.measure_style(a.label, None, &body_style);
                let bw = (f64::from(tw) + ACTION_PAD_X * scale).max(ACTION_MIN_W * scale);
                let r = Rect::new(bx - bw, by, bx, by + button_h);
                let Some(action_rect) = actions.get_mut(index) else {
                    continue;
                };
                *action_rect = r;
                bx = r.x0 - ACTION_GAP * scale;
            }
        }

        let primary = spec.actions.iter().position(|a| {
            matches!(a.emphasis, ModalEmphasis::Primary | ModalEmphasis::Danger) && a.enabled
        });

        ModalLayout {
            scrim: viewport,
            card,
            title,
            close,
            body,
            actions,
            body_overflows,
            primary,
            dismiss_on_scrim: spec.dismiss_on_scrim,
        }
    }

    /// Paint the scrim, the card, the title and the close control. Call before
    /// drawing the body.
    pub fn paint_backdrop(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &ModalLayout,
        spec: &ModalSpec<'_>,
    ) {
        let tokens = &theme.tokens;
        let (ink, _dim) = Self::ink(theme);

        crate::chrome::fill_scrim(scene, transform, layout.scrim, MODAL_SCRIM_ALPHA, theme);

        let scale = Self::chrome_scale(theme);
        let radius = f64::from(tokens.radii.control) * scale;
        // Top of the elevation ramp: a modal is the one surface that is
        // unambiguously above everything, and reading as *lifted* is what
        // separates it from a panel that happens to be centred.
        crate::surface::shadow(
            scene,
            transform,
            layout.card,
            radius,
            &tokens.elevation.floating,
        );
        let card = RoundedRect::from_rect(layout.card, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            Self::card_fill(theme),
            None,
            &card,
        );
        // The derived panel edge (HIG §9.5), never a declared
        // `border_standard` that sat within rounding distance of the fill.
        scene.stroke(
            &Stroke::new(1.0 * scale),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &card,
        );

        if !spec.title.is_empty() {
            let style = tokens.typography.style(TextRole::Body);
            // Title-band width is a bounded screen dimension; the f64->f32
            // narrowing for the text layout call cannot lose meaningful
            // precision at UI scale.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bounded screen-space width; f32 precision is sufficient for text layout"
            )]
            let title_w = layout.title.width() as f32;
            let shown = text_renderer.elide_to_width_style(spec.title, title_w, &style);
            let (_, th) = text_renderer.measure_style(&shown, None, &style);
            let ty = f64::midpoint(layout.title.y0, layout.title.y1) - f64::from(th) / 2.0;
            // Affine composition of f64 transforms via the `Mul` operator
            // overload; clippy's arithmetic_side_effects flags any operator
            // overload, but this cannot meaningfully overflow.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine::mul composes f64 transform matrices; cannot meaningfully overflow"
            )]
            let pos = transform * Affine::translate((layout.title.x0, ty));
            text_renderer.draw_style(
                scene,
                &shown,
                ink,
                pos,
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
        }

        // A real button face rather than a hand-drawn glyph, so the close
        // control gets the ghost fill, the hover/press states and the focus
        // ring every other control has.
        if let Some(c) = layout.close {
            let face = ButtonFace::new(c, "")
                .role(ButtonRole::Plain)
                .icon(Icon::X)
                .hovered(spec.close_hovered)
                .focused(spec.close_focused)
                .accessible_name("Close");
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
    }

    /// Paint the action row. Call after drawing the body.
    pub fn paint_actions(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &ModalLayout,
        spec: &ModalSpec<'_>,
    ) {
        for (i, a) in spec.actions.iter().enumerate() {
            let Some(rect) = layout.actions.get(i).copied() else {
                continue;
            };
            let face = ButtonFace::new(rect, a.label);
            // A modal's danger action is the confirmation step, the one
            // place a destructive button is filled.
            let face = match a.emphasis {
                ModalEmphasis::Primary => face.role(ButtonRole::Primary),
                ModalEmphasis::Secondary => face.role(ButtonRole::Secondary),
                ModalEmphasis::Danger => face.role(ButtonRole::Destructive).confirming(),
            }
            .hovered(a.hovered && a.enabled)
            .pressed(a.pressed && a.enabled)
            .focused(a.focused && a.enabled)
            .disabled(!a.enabled);
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
    }
}

#[cfg(test)]
mod tests {
    //! The geometry, and the two guarantees that made this widget exist: there
    //! is always a visible way out, and the primary action's label is readable
    //! on every palette the OS ships.

    use super::*;
    use crate::a11y::contrast_ratio;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);

    fn themes() -> Vec<(&'static str, Theme)> {
        vec![("dark", Theme::dark()), ("light", Theme::light())]
    }

    fn lay(spec: &ModalSpec<'_>, theme: &Theme) -> ModalLayout {
        let mut tr = TextRenderer::new();
        ModalFrame::layout(VIEWPORT, spec, &mut tr, theme)
    }

    #[test]
    fn card_sizes_to_its_body_within_viewport_margins() {
        // Case `card_centres_on_the_viewport`.
        {
            let spec = ModalSpec::new("Workspace invite").body_height(120.0);
            let l = lay(&spec, &Theme::dark());
            let cx = f64::midpoint(l.card.x0, l.card.x1);
            let cy = f64::midpoint(l.card.y0, l.card.y1);
            assert!(
                (cx - 800.0).abs() < 0.5,
                "card centre x = {cx}, viewport centre is 800"
            );
            assert!(
                (cy - 500.0).abs() < 0.5,
                "card centre y = {cy}, viewport centre is 500"
            );
        }

        // Case `an_oversized_body_clamps_the_card_and_reports_the_overflow`.
        // A body taller than the screen clamps the card to the viewport and says
        // so, rather than drawing an action row off the bottom edge.
        {
            let actions = [ModalAction::primary("OK")];
            let spec = ModalSpec::new("Tall").actions(&actions).body_height(9000.0);
            let l = lay(&spec, &Theme::dark());
            assert!(
                l.body_overflows,
                "a 9000px body in a 1000px viewport must report overflow"
            );
            assert!(
                l.card.y0 >= VIEWPORT.y0 && l.card.y1 <= VIEWPORT.y1,
                "the card {:?} escaped the viewport {:?}",
                l.card,
                VIEWPORT,
            );
            let action = l.actions[0];
            assert!(
                VIEWPORT.contains(action.center()),
                "the action row at {action:?} is off screen — nobody can reach it",
            );
        }

        // Case `a_body_that_fits_does_not_report_overflow`.
        {
            let spec = ModalSpec::new("Short").body_height(100.0);
            assert!(!lay(&spec, &Theme::dark()).body_overflows);
        }

        // Case `the_card_keeps_its_margin_on_a_small_output`.
        // The card never touches an edge, even on an output smaller than the
        // card's own maximum.
        {
            let small = Rect::new(0.0, 0.0, 420.0, 360.0);
            let spec = ModalSpec::new("Workspace invite").body_height(600.0);
            let mut tr = TextRenderer::new();
            let l = ModalFrame::layout(small, &spec, &mut tr, &Theme::dark());
            assert!(l.card.x0 >= small.x0, "card runs off the left edge");
            assert!(l.card.x1 <= small.x1, "card runs off the right edge");
            assert!(l.card.y0 >= small.y0, "card runs off the top edge");
            assert!(l.card.y1 <= small.y1, "card runs off the bottom edge");
            assert!(
                l.card.width() <= small.width() - MODAL_VIEWPORT_MARGIN * 2.0 + 0.5,
                "card width {} leaves no margin in a {}-wide output",
                l.card.width(),
                small.width(),
            );
        }

        // Case `the_card_stops_growing_at_its_maximum`.
        {
            let wide = Rect::new(0.0, 0.0, 3840.0, 2160.0);
            let spec = ModalSpec::new("Workspace invite").body_height(200.0);
            let mut tr = TextRenderer::new();
            let l = ModalFrame::layout(wide, &spec, &mut tr, &Theme::dark());
            assert!(
                l.card.width() <= MODAL_MAX_WIDTH + 0.5,
                "card is {} wide on a 4K output; the cap is {MODAL_MAX_WIDTH}",
                l.card.width(),
            );
        }

        // Case `a_modal_without_actions_gives_the_room_to_its_body`.
        // A modal with no actions has no action row, and its body takes the space
        // back.
        {
            let actions = [ModalAction::primary("OK")];
            let with = ModalSpec::new("T").actions(&actions).body_height(100.0);
            let without = ModalSpec::new("T").body_height(100.0);
            let theme = Theme::dark();
            let a = lay(&with, &theme);
            let b = lay(&without, &theme);
            assert!(b.actions.is_empty());
            assert!(
                b.card.height() < a.card.height(),
                "an action-less modal ({}) should be shorter than one with a row ({})",
                b.card.height(),
                a.card.height(),
            );
        }

        // Case `body_width_is_what_the_layout_hands_back`.
        // The width a caller predicts before measuring its rows is the width
        // the layout hands back — on every scale axis a theme can carry, and
        // on a viewport too small for the card's natural size.
        {
            let actions = [ModalAction::primary("Continue")];
            let cases = [
                ("dark", Theme::dark(), VIEWPORT),
                ("light", Theme::light(), VIEWPORT),
                ("text-1.5", Theme::dark().with_text_scale(1.5), VIEWPORT),
                ("ui-1.25", Theme::dark().with_ui_scale(1.25), VIEWPORT),
                (
                    "small output",
                    Theme::dark(),
                    Rect::new(0.0, 0.0, 300.0, 240.0),
                ),
            ];
            for (name, theme, viewport) in cases {
                for max_width in [MODAL_MAX_WIDTH, 560.0] {
                    let spec = ModalSpec::new("Title")
                        .actions(&actions)
                        .max_width(max_width)
                        .body_height(120.0);
                    let predicted = ModalFrame::body_width(viewport, &spec, &theme);
                    let mut tr = TextRenderer::new();
                    let l = ModalFrame::layout(viewport, &spec, &mut tr, &theme);
                    assert!(
                        (l.body.width() - predicted).abs() < 0.01,
                        "{name} @ max {max_width}: predicted {predicted}, layout gave {}",
                        l.body.width(),
                    );
                    assert!(
                        (l.card.width() - ModalFrame::card_width(viewport, &spec, &theme)).abs()
                            < 0.01,
                        "{name} @ max {max_width}: card_width disagrees with the layout",
                    );
                }
            }
        }
    }

    #[test]
    fn modal_controls_are_hittable_and_show_their_states() {
        // Case `a_closable_modal_has_a_close_control_you_can_hit`.
        // A closable modal always has a close control, and it is a real hit
        // target rather than a decorative glyph.
        {
            let spec = ModalSpec::new("Workspace invite").body_height(120.0);
            for (name, theme) in themes() {
                let l = lay(&spec, &theme);
                let close = l
                    .close
                    .unwrap_or_else(|| panic!("{name}: no close control"));
                assert!(
                    close.width() >= 28.0 && close.height() >= 28.0,
                    "{name}: close target is {}x{} — too small to hit",
                    close.width(),
                    close.height(),
                );
                assert!(
                    l.card.contains(close.center()),
                    "{name}: close control is outside the card",
                );
                assert_eq!(
                    l.hit(close.center()),
                    Some(ModalHit::Close),
                    "{name}: the close control does not hit-test as Close",
                );
                assert!(
                    l.dismisses(close.center()),
                    "{name}: clicking close does not dismiss"
                );
            }
        }

        // Case `an_action_shows_hover_press_and_focus`.
        // A modal action reacts. A focused one in particular has to draw the
        // ring, because a modal takes the keyboard and a keyboard user needs to
        // see where they are in it.
        {
            let theme = Theme::dark();

            let paint = |a: ModalAction<'_>| {
                let actions = [a];
                let spec = ModalSpec::new("Invite someone")
                    .actions(&actions)
                    .body_height(80.0);
                let mut tr = TextRenderer::new();
                let layout = ModalFrame::layout(VIEWPORT, &spec, &mut tr, &theme);
                let mut scene = Scene::new();
                ModalFrame::paint_actions(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &layout,
                    &spec,
                );
                scene.encoding().draw_data.clone()
            };

            let rest = paint(ModalAction::secondary("Cancel"));
            for (what, face) in [
                ("hover", ModalAction::secondary("Cancel").hovered(true)),
                ("press", ModalAction::secondary("Cancel").pressed(true)),
                ("focus", ModalAction::secondary("Cancel").focused(true)),
            ] {
                assert_ne!(
                    paint(face),
                    rest,
                    "a {what}ed action paints the same as one at rest — the \
                 state never reached the face",
                );
            }

            // Hover on something that cannot be clicked is a lie about what will
            // happen.
            assert_eq!(
                paint(
                    ModalAction::secondary("Cancel")
                        .enabled(false)
                        .hovered(true)
                ),
                paint(ModalAction::secondary("Cancel").enabled(false)),
                "a disabled action must not react to the pointer",
            );
        }

        // Case `the_close_control_shows_hover_and_focus`.
        // The close control reacts too. It is the visible way out of a modal, and
        // one that does not respond to the pointer reads as decoration.
        {
            let theme = Theme::light();
            let paint = |spec: &ModalSpec<'_>| {
                let mut tr = TextRenderer::new();
                let layout = ModalFrame::layout(VIEWPORT, spec, &mut tr, &theme);
                let mut scene = Scene::new();
                ModalFrame::paint_backdrop(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &layout,
                    spec,
                );
                scene.encoding().draw_data.clone()
            };
            let base = ModalSpec::new("Invite someone").body_height(80.0);
            let rest = paint(&base);
            assert_ne!(paint(&base.clone().close_hovered(true)), rest, "hover");
            assert_ne!(paint(&base.clone().close_focused(true)), rest, "focus");
        }

        // Case `the_title_band_stops_before_the_close_control`.
        {
            let spec = ModalSpec::new("A title long enough to want the whole row and then some");
            let l = lay(&spec, &Theme::dark());
            let close = l.close.expect("closable by default");
            assert!(
                l.title.x1 <= close.x0,
                "title band ends at {} but the close control starts at {}",
                l.title.x1,
                close.x0,
            );
        }
    }

    #[test]
    fn actions_follow_platform_order_inside_the_card() {
        // Case `linux_actions_are_left_to_right_with_the_last_at_the_right_edge`.
        // The default Linux convention puts the primary action at the trailing
        // edge while preserving authored indices for event dispatch.
        {
            let actions = [
                ModalAction::secondary("Cancel"),
                ModalAction::primary("Create invite"),
            ];
            let spec = ModalSpec::new("Workspace invite")
                .actions(&actions)
                .platform_conventions(crate::platform::PlatformConventions::new(
                    crate::platform::Platform::Linux,
                ))
                .body_height(100.0);
            let l = lay(&spec, &Theme::dark());
            assert_eq!(l.actions.len(), 2);
            assert!(
                l.actions[0].x1 <= l.actions[1].x0,
                "Cancel ({:?}) should sit left of Create invite ({:?})",
                l.actions[0],
                l.actions[1],
            );
            assert!(
                (l.actions[1].x1 - (l.card.x1 - MODAL_PAD)).abs() < 0.5,
                "the last action should hug the card's right padding; got {} vs {}",
                l.actions[1].x1,
                l.card.x1 - MODAL_PAD,
            );
            assert_eq!(l.primary, Some(1), "Enter should fire the primary action");
        }

        // Case `action_geometry_follows_platform_order_without_changing_indices`.
        {
            use crate::platform::{Platform, PlatformConventions};

            // Intentionally author primary first: semantic emphasis, rather than
            // slice position, determines visual order.
            let actions = [
                ModalAction::primary("Save"),
                ModalAction::secondary("Cancel"),
            ];
            let mac = ModalSpec::new("Save changes?")
                .actions(&actions)
                .platform_conventions(PlatformConventions::new(Platform::MacOs));
            let mac_layout = lay(&mac, &Theme::dark());
            assert!(mac_layout.actions[0].x0 > mac_layout.actions[1].x0);
            assert_eq!(
                mac_layout.hit(mac_layout.actions[0].center()),
                Some(ModalHit::Action(0))
            );

            let windows = ModalSpec::new("Save changes?")
                .actions(&actions)
                .platform_conventions(PlatformConventions::new(Platform::Windows));
            let windows_layout = lay(&windows, &Theme::dark());
            assert!(windows_layout.actions[0].x0 < windows_layout.actions[1].x0);
            assert_eq!(
                windows_layout.hit(windows_layout.actions[0].center()),
                Some(ModalHit::Action(0))
            );
        }

        // Case `every_action_is_inside_the_card_and_hit_tests_to_itself`.
        {
            let actions = [
                ModalAction::secondary("Paste"),
                ModalAction::secondary("Cancel"),
                ModalAction::primary("Join"),
            ];
            let spec = ModalSpec::new("Join a workspace")
                .actions(&actions)
                .body_height(120.0);
            let l = lay(&spec, &Theme::dark());
            for (i, r) in l.actions.iter().enumerate() {
                assert!(r.area() > 0.0, "action {i} has no area");
                assert!(
                    l.card.contains(r.center()),
                    "action {i} at {r:?} is outside the card {:?}",
                    l.card,
                );
                assert_eq!(l.hit(r.center()), Some(ModalHit::Action(i)));
                assert!(
                    !l.dismisses(r.center()),
                    "an action click must not dismiss on its own"
                );
            }
        }

        // Case `a_disabled_action_is_not_the_default_action`.
        {
            let actions = [ModalAction::primary("Join").enabled(false)];
            let spec = ModalSpec::new("Join a workspace")
                .actions(&actions)
                .body_height(80.0);
            let l = lay(&spec, &Theme::dark());
            assert_eq!(
                l.primary, None,
                "a disabled action must not be what Enter fires"
            );
        }

        // Case `action_height_is_the_height_of_every_action_rect`.
        // `action_height` is the height of every action rect the layout
        // produces, so a body control sized from it shares the row's line.
        {
            let actions = [
                ModalAction::secondary("Back"),
                ModalAction::primary("Continue"),
            ];
            for theme in [Theme::dark(), Theme::dark().with_text_scale(2.0)] {
                let spec = ModalSpec::new("Title").actions(&actions).body_height(80.0);
                let l = lay(&spec, &theme);
                let expected = ModalFrame::action_height(&theme);
                assert!(expected > 0.0);
                for (i, r) in l.actions.iter().enumerate() {
                    assert!(
                        (r.height() - expected).abs() < 0.01,
                        "text_scale {}: action {i} is {} tall, action_height says {expected}",
                        theme.text_scale,
                        r.height(),
                    );
                }
            }
        }
    }

    #[test]
    fn scrim_and_card_route_clicks_that_hit_no_control() {
        // Case `the_scrim_covers_everything_and_only_dismisses_when_asked`.
        // The scrim covers the viewport, and a click on it dismisses only when
        // the spec allows — but is never passed through either way.
        {
            let corner = Point::new(4.0, 4.0);

            let spec = ModalSpec::new("Workspace invite").body_height(100.0);
            let l = lay(&spec, &Theme::dark());
            assert_eq!(l.scrim, VIEWPORT, "the scrim must cover the whole viewport");
            assert_eq!(l.hit(corner), Some(ModalHit::Scrim));
            assert!(
                l.dismisses(corner),
                "the default modal dismisses on a scrim click"
            );

            let sticky = ModalSpec::new("Workspace invite")
                .body_height(100.0)
                .dismiss_on_scrim(false);
            let l = lay(&sticky, &Theme::dark());
            assert_eq!(
                l.hit(corner),
                Some(ModalHit::Scrim),
                "a sticky modal still swallows the click — it just does not close",
            );
            assert!(!l.dismisses(corner));
        }

        // Case `the_card_swallows_clicks_that_hit_no_control`.
        {
            let spec = ModalSpec::new("Workspace invite").body_height(160.0);
            let l = lay(&spec, &Theme::dark());
            let p = l.body.center();
            assert_eq!(l.hit(p), Some(ModalHit::Card));
            assert!(!l.dismisses(p), "clicking the body must never dismiss");
        }

        // Case `a_point_off_the_viewport_hits_nothing`.
        {
            let spec = ModalSpec::new("T").body_height(100.0);
            let l = lay(&spec, &Theme::dark());
            assert_eq!(l.hit(Point::new(-10.0, -10.0)), None);
        }
    }

    #[test]
    fn modal_ink_and_action_labels_are_legible_on_every_theme() {
        // Case `modal_ink_is_legible_on_every_theme`.
        // The modal's own ink clears AA against the card it is painted on, on
        // every theme the OS ships.
        {
            for (name, theme) in themes() {
                let card = ModalFrame::card_fill(&theme);
                let (ink, dim) = ModalFrame::ink(&theme);
                let r = contrast_ratio(ink, card);
                assert!(
                    r >= MODAL_MIN_INK_CONTRAST,
                    "{name}: modal ink contrast is {r:.2}:1 against the card — AA needs {MODAL_MIN_INK_CONTRAST}",
                );
                // The dim tier is secondary, so it is held to the large-text bar
                // rather than the body bar.
                let rd = contrast_ratio(dim, card);
                assert!(
                    rd >= 3.0,
                    "{name}: dim ink contrast is {rd:.2}:1 — unreadable as a sub-label"
                );
            }
        }

        // Case `every_action_label_is_legible_on_its_own_fill`.
        // Every action label is legible on the fill it is painted on, on every
        // theme and in every emphasis — asserted on the colour the shared face
        // actually resolves, so it keeps holding when the accent ramp is re-themed.
        {
            let styles = [
                (ModalEmphasis::Primary, ButtonRole::Primary),
                (ModalEmphasis::Secondary, ButtonRole::Secondary),
                (ModalEmphasis::Danger, ButtonRole::Destructive),
            ];
            for (name, theme) in themes() {
                for (emphasis, style) in styles {
                    let face = ButtonFace::new(Rect::new(0.0, 0.0, 100.0, 34.0), "Create invite");
                    let face = if style == ButtonRole::Destructive {
                        face.role(style).confirming()
                    } else {
                        face.role(style)
                    };
                    let (bg, _border, painted) = face.colors(&theme);
                    let r = contrast_ratio(painted, bg);
                    assert!(
                        r >= 4.5,
                        "{name}/{emphasis:?}: label contrast is {r:.2}:1 on its fill — AA needs 4.5",
                    );
                    // Never worse than plain chrome ink would have been.
                    let naive = contrast_ratio(theme.text_primary, bg);
                    assert!(
                        r >= naive,
                        "{name}/{emphasis:?}: measured label ({r:.2}:1) is worse than chrome ink ({naive:.2}:1)",
                    );
                }
            }
        }
    }
}
