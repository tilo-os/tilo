//! `ModalFrame` — the one modal chrome in qOS.
//!
//! Before this existed every modal surface in the system drew its own
//! rounded rect, its own title, and its own row of buttons. The shell's
//! workspace-invite card is the case that forced the issue: a hand-drawn
//! card with hand-drawn button rects, no scrim, no close control, and a
//! primary button that painted `text_primary` on an accent fill — which
//! is legible on the default palette and is not on every other one. None
//! of that is a tidiness problem. A hand-rolled control has no focus
//! ring, no disabled state, no accessible name, and no guarantee that
//! its label can be read.
//!
//! So this is deliberately **not** a second modal widget. It is the
//! chrome — scrim, card, title row, close control, action row — factored
//! so that both kinds of caller share one implementation:
//!
//! * **Retained widgets** ([`ModalDialog`](super::modal_dialog::ModalDialog))
//!   lay out with taffy and paint through the `Widget` trait.
//! * **Immediate-mode painters** (the compositor's `overlay_host`, which
//!   owns its own hit-testing and paints straight into a `Scene`) call
//!   [`ModalFrame::layout`] for geometry and the two paint entry points
//!   around their own body.
//!
//! The action row goes through [`Button::paint`] and [`ButtonFace`], so a
//! modal button *is* a `Button` — same fills, same hover/press/focus/
//! disabled states, same accent-safe label that keeps a primary action
//! readable on every palette.
//!
//! # Painting order
//!
//! The body sits between the two paint calls, because the card has to be
//! under the body while the action row is laid out from the bottom up:
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

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::button::{Button, ButtonFace, ButtonStyle};

/// Alpha of the scrim painted over everything behind the modal.
///
/// 0.45 rather than the 0.30 a design tool would specify. vello has no
/// backdrop blur, so the scrim is the *only* thing separating the card
/// from the desktop; an un-blurred 0.30 lets enough structure through
/// that the card reads as floating over noise. 0.45 is the alpha at
/// which an un-blurred scrim reads about the same as a blurred 0.30 —
/// the compositor arrived at the same number independently for its
/// settings modal, and this is that number, in one place.
pub const MODAL_SCRIM_ALPHA: f32 = 0.45;

/// Widest a modal card is drawn. Past this a single column of text stops
/// being one column and starts being a paragraph nobody tracks across.
pub const MODAL_MAX_WIDTH: f64 = 520.0;
/// Narrowest a modal card is drawn before it starts eliding instead.
pub const MODAL_MIN_WIDTH: f64 = 320.0;
/// Gap kept between the card and every edge of the viewport, so the card
/// never reaches an edge even on a small output.
pub const MODAL_VIEWPORT_MARGIN: f64 = 24.0;
/// Height of the title row — title plus the close control's hit target.
pub const MODAL_TITLE_ROW_H: f64 = 52.0;
/// Height of the action row, including the padding above and below it.
pub const MODAL_ACTION_ROW_H: f64 = 56.0;
/// Padding from the card edge to its content, on every side.
pub const MODAL_PAD: f64 = 20.0;
/// Side of the square close-control hit target. A 32-px target with a
/// 14-px glyph in it: the glyph is the size an `X` wants to be, and the
/// target is the size a thing you have to hit wants to be.
pub const MODAL_CLOSE_SIZE: f64 = 32.0;

/// Height of one action button. Matches the compact chrome button height
/// the rest of the overlay surfaces use.
const ACTION_H: f64 = 34.0;
/// Gap between two action buttons.
const ACTION_GAP: f64 = 8.0;
/// Narrowest an action button is laid out, so that `OK` and `Copy` are
/// still buttons rather than badges.
const ACTION_MIN_W: f64 = 88.0;
/// Horizontal padding added around an action's measured label.
const ACTION_PAD_X: f64 = 28.0;
/// Contrast a modal's own ink must clear against the card it sits on
/// before it is used as-is. WCAG 2.1 AA for body text.
const MODAL_MIN_INK_CONTRAST: f64 = 4.5;

/// How an action is weighted in the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalEmphasis {
    /// The default action. One per modal; painted accent-filled, and the
    /// one Enter fires.
    Primary,
    /// Everything else — Cancel, and any additional verb.
    Secondary,
    /// A primary action that destroys something. Painted with the danger
    /// token, and still the action Enter would fire.
    Danger,
}

/// One button in the action row.
#[derive(Clone, Copy, Debug)]
pub struct ModalAction<'a> {
    pub label: &'a str,
    pub emphasis: ModalEmphasis,
    /// A disabled action is painted dimmed and is not hit-testable.
    pub enabled: bool,
}

impl<'a> ModalAction<'a> {
    pub fn primary(label: &'a str) -> Self {
        Self { label, emphasis: ModalEmphasis::Primary, enabled: true }
    }
    pub fn secondary(label: &'a str) -> Self {
        Self { label, emphasis: ModalEmphasis::Secondary, enabled: true }
    }
    pub fn danger(label: &'a str) -> Self {
        Self { label, emphasis: ModalEmphasis::Danger, enabled: true }
    }
    pub fn enabled(mut self, v: bool) -> Self {
        self.enabled = v;
        self
    }
}

/// What a modal is being asked to frame.
#[derive(Clone, Debug)]
pub struct ModalSpec<'a> {
    /// Shown in the title row. A modal without a title is a modal whose
    /// purpose the person has to infer from its body.
    pub title: &'a str,
    /// Draw a close control in the title row. Escape always dismisses;
    /// this is the visible half of that, and it is on by default because
    /// a surface with no way out but a key nobody can see is the defect
    /// this widget exists to stop repeating.
    pub closable: bool,
    /// Actions in **visual left-to-right order**. The group is right
    /// aligned, so the last entry lands at the right edge — put the
    /// primary action last, which is where every platform puts it.
    pub actions: &'a [ModalAction<'a>],
    /// Height the caller's body content wants, in logical pixels.
    pub body_height: f64,
    /// Widest the card may be drawn. Defaults to [`MODAL_MAX_WIDTH`].
    pub max_width: f64,
    /// Whether a click on the scrim dismisses. The scrim **always**
    /// swallows the click either way — a surface that lets clicks reach
    /// what is behind it is not modal. Turn this off for a modal that
    /// would lose work if a stray click closed it.
    pub dismiss_on_scrim: bool,
}

impl<'a> Default for ModalSpec<'a> {
    fn default() -> Self {
        Self {
            title: "",
            closable: true,
            actions: &[],
            body_height: 0.0,
            max_width: MODAL_MAX_WIDTH,
            dismiss_on_scrim: true,
        }
    }
}

impl<'a> ModalSpec<'a> {
    pub fn new(title: &'a str) -> Self {
        Self { title, ..Self::default() }
    }
    pub fn actions(mut self, actions: &'a [ModalAction<'a>]) -> Self {
        self.actions = actions;
        self
    }
    pub fn body_height(mut self, h: f64) -> Self {
        self.body_height = h;
        self
    }
    pub fn max_width(mut self, w: f64) -> Self {
        self.max_width = w;
        self
    }
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    pub fn dismiss_on_scrim(mut self, v: bool) -> Self {
        self.dismiss_on_scrim = v;
        self
    }
}

/// Where every part of the modal landed. Pure geometry — computed
/// without touching a GPU, which is what lets the layout rules be tested
/// directly rather than inferred from a rendered frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ModalLayout {
    /// The whole viewport. The scrim covers this.
    pub scrim: Rect,
    /// The card.
    pub card: Rect,
    /// The band the title is drawn in — from the card's left padding to
    /// wherever the close control starts.
    pub title: Rect,
    /// The close control's hit target, when the spec asked for one.
    pub close: Option<Rect>,
    /// The region the caller owns. Content taller than this must scroll
    /// or elide inside it — see [`Self::body_overflows`].
    pub body: Rect,
    /// One rect per entry in `spec.actions`, in the same order.
    pub actions: Vec<Rect>,
    /// True when the body was clamped: the caller asked for more height
    /// than the viewport can give a centred card. The card does not grow
    /// past the viewport; the caller scrolls.
    pub body_overflows: bool,
    /// Index of the action Enter should fire, if any.
    pub primary: Option<usize>,
    /// Mirrors [`ModalSpec::dismiss_on_scrim`], so a caller holding only
    /// the layout can still answer a scrim click correctly.
    pub dismiss_on_scrim: bool,
}

/// What a point landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalHit {
    /// The close control.
    Close,
    /// An action, by index into `spec.actions`.
    Action(usize),
    /// Somewhere inside the card that is not a control. Swallow it.
    Card,
    /// The scrim. Dismiss when [`ModalLayout::dismiss_on_scrim`],
    /// otherwise swallow.
    Scrim,
}

impl ModalLayout {
    /// Resolve a point to whatever it hit.
    ///
    /// Ordered controls-first so an action that overlaps the card (all of
    /// them do) resolves to the action. Returns `None` only for a point
    /// outside the viewport entirely.
    pub fn hit(&self, p: Point) -> Option<ModalHit> {
        if let Some(c) = self.close {
            if c.contains(p) {
                return Some(ModalHit::Close);
            }
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

    /// Does a click at `p` dismiss the modal?
    ///
    /// True for the close control, and for the scrim when the spec allows
    /// it. Everything else is swallowed.
    pub fn dismisses(&self, p: Point) -> bool {
        match self.hit(p) {
            Some(ModalHit::Close) => true,
            Some(ModalHit::Scrim) => self.dismiss_on_scrim,
            _ => false,
        }
    }
}

/// The modal chrome. A namespace, not a state machine: every entry point
/// is a free function over `(viewport, spec)`, so an immediate-mode
/// caller keeps owning its own state.
pub struct ModalFrame;

impl ModalFrame {
    /// The card's own fill.
    pub fn card_fill(theme: &Theme) -> Color {
        theme.tokens.colors.surfaces.raised
    }

    /// Ink that is legible on the modal card, as `(primary, dim)`.
    ///
    /// The chrome's own `text_primary` is used when it clears AA against
    /// the card, and the contrast-guaranteed on-card ramp is used when it
    /// does not. Modals were the surface where taking a fixed ramp slot
    /// went wrong — a slot that reads on one palette can vanish on
    /// another, and a label nobody can read is the same defect as a label
    /// that is not painted.
    pub fn ink(theme: &Theme) -> (Color, Color) {
        let card = Self::card_fill(theme);
        if crate::a11y::contrast_ratio(theme.text_primary, card) >= MODAL_MIN_INK_CONTRAST {
            (theme.text_primary, theme.text_dim)
        } else {
            let on = theme.on_card();
            (on.ink, on.ink_dim)
        }
    }

    /// The most legible label colour available for text on `bg`.
    ///
    /// A filled button's label is the one piece of text in a modal whose
    /// background is not the card but the accent — and the accent ramp is
    /// themeable, so the answer is not a constant. On the palette this
    /// was written against, the shared style's white label scores 3.17:1
    /// on the accent fill, which is under AA for text this size; the dark
    /// end of the neutral ramp scores 6.6:1 on the same fill. So the
    /// colour is *measured* rather than assumed, and the ramp ends are
    /// offered alongside white so the winner is a themed neutral rather
    /// than pure black.
    pub fn ink_on(bg: Color, theme: &Theme) -> Color {
        use crate::a11y::contrast_ratio;
        let n = &theme.tokens.colors.neutral;
        [Color::WHITE, n[0], n[1], n[9], n[10]]
            .into_iter()
            .max_by(|a, b| {
                contrast_ratio(*a, bg)
                    .partial_cmp(&contrast_ratio(*b, bg))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Color::WHITE)
    }

    /// Compute the geometry for `spec` inside `viewport`.
    pub fn layout(
        viewport: Rect,
        spec: &ModalSpec<'_>,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> ModalLayout {
        let tokens = &theme.tokens;
        let body_size = tokens.typography.scale.sm.max(12.0);

        // --- width -------------------------------------------------
        let avail_w = (viewport.width() - MODAL_VIEWPORT_MARGIN * 2.0).max(0.0);
        let w = spec.max_width.min(avail_w).max(MODAL_MIN_WIDTH.min(avail_w));

        // --- height ------------------------------------------------
        let action_h = if spec.actions.is_empty() { 0.0 } else { MODAL_ACTION_ROW_H };
        let chrome_h = MODAL_TITLE_ROW_H + action_h + MODAL_PAD;
        let wanted_h = chrome_h + spec.body_height.max(0.0);
        let avail_h = (viewport.height() - MODAL_VIEWPORT_MARGIN * 2.0).max(0.0);
        let h = wanted_h.min(avail_h);
        // The card never grows past the viewport. When the body asked for
        // more than fits, say so rather than drawing a card whose bottom
        // is off screen — an action row nobody can reach is worse than a
        // body that scrolls.
        let body_overflows = wanted_h > avail_h + 0.5;

        let cx = (viewport.x0 + viewport.x1) / 2.0;
        let cy = (viewport.y0 + viewport.y1) / 2.0;
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);

        // --- title row ---------------------------------------------
        let close = if spec.closable {
            let x1 = card.x1 - MODAL_PAD + 6.0;
            let mid = card.y0 + MODAL_TITLE_ROW_H / 2.0;
            Some(Rect::new(
                x1 - MODAL_CLOSE_SIZE,
                mid - MODAL_CLOSE_SIZE / 2.0,
                x1,
                mid + MODAL_CLOSE_SIZE / 2.0,
            ))
        } else {
            None
        };
        // The title stops where the close control starts, so a long title
        // elides instead of running under the X.
        let title_x1 = close.map(|c| c.x0 - 10.0).unwrap_or(card.x1 - MODAL_PAD);
        let title = Rect::new(
            card.x0 + MODAL_PAD,
            card.y0,
            title_x1.max(card.x0 + MODAL_PAD),
            card.y0 + MODAL_TITLE_ROW_H,
        );

        // --- body --------------------------------------------------
        let body = Rect::new(
            card.x0 + MODAL_PAD,
            card.y0 + MODAL_TITLE_ROW_H,
            card.x1 - MODAL_PAD,
            (card.y1 - action_h - MODAL_PAD).max(card.y0 + MODAL_TITLE_ROW_H),
        );

        // --- action row --------------------------------------------
        //
        // Laid out right-to-left so the group hugs the right edge, then
        // reversed, so `actions[i]` matches `spec.actions[i]` and the
        // last entry is the one at the right edge.
        let mut actions: Vec<Rect> = Vec::with_capacity(spec.actions.len());
        if !spec.actions.is_empty() {
            let by = card.y1 - MODAL_PAD - ACTION_H;
            let mut bx = card.x1 - MODAL_PAD;
            for a in spec.actions.iter().rev() {
                let (tw, _) = text_renderer.measure(a.label, body_size, None);
                let bw = (tw as f64 + ACTION_PAD_X).max(ACTION_MIN_W);
                let r = Rect::new(bx - bw, by, bx, by + ACTION_H);
                actions.push(r);
                bx = r.x0 - ACTION_GAP;
            }
            actions.reverse();
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

    /// Paint the scrim, the card, the title and the close control.
    ///
    /// Call before drawing the body.
    pub fn paint_backdrop(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &ModalLayout,
        spec: &ModalSpec<'_>,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (ink, _dim) = Self::ink(theme);

        // --- scrim -------------------------------------------------
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.scrim.with_alpha_factor(MODAL_SCRIM_ALPHA),
            None,
            &layout.scrim,
        );

        // --- card --------------------------------------------------
        let radius = tokens.radii.lg as f64;
        // Top of the elevation ramp: a modal is the one surface that is
        // unambiguously above everything, and reading as *lifted* is what
        // separates it from a panel that happens to be centred.
        crate::surface::shadow(scene, transform, layout.card, radius, &tokens.elevation.e4);
        let card = RoundedRect::from_rect(layout.card, radius);
        scene.fill(Fill::NonZero, transform, Self::card_fill(theme), None, &card);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &card);

        // --- title -------------------------------------------------
        if !spec.title.is_empty() {
            let size = tokens.typography.scale.base.max(14.0);
            let shown = text_renderer.elide_to_width(spec.title, size, layout.title.width() as f32);
            let (_, th) = text_renderer.measure(&shown, size, None);
            let ty = (layout.title.y0 + layout.title.y1) / 2.0 - th as f64 / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &shown,
                size,
                ink,
                transform * Affine::translate((layout.title.x0, ty)),
                None,
                TextWeight::Semibold,
            );
        }

        // --- close control -----------------------------------------
        //
        // A real button face, not a hand-drawn glyph: it gets the ghost
        // fill, the hover/press states and the focus ring every other
        // control in the system has, because it *is* one.
        if let Some(c) = layout.close {
            let face = ButtonFace::new(c, "")
                .style(ButtonStyle::Ghost)
                .icon(Icon::X)
                .radius(tokens.radii.sm as f64)
                .accessible_name("Close");
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
    }

    /// Paint the action row.
    ///
    /// Call after drawing the body. Every button goes through
    /// [`Button::paint`], so the primary action's label is whatever the
    /// shared `face_colors` resolves against the accent fill rather than
    /// a chrome ink that may or may not read on it.
    pub fn paint_actions(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &ModalLayout,
        spec: &ModalSpec<'_>,
    ) {
        let body_size = theme.tokens.typography.scale.sm.max(12.0);
        for (i, a) in spec.actions.iter().enumerate() {
            let Some(rect) = layout.actions.get(i).copied() else {
                continue;
            };
            let style = match a.emphasis {
                ModalEmphasis::Primary => ButtonStyle::Primary,
                ModalEmphasis::Secondary => ButtonStyle::Neutral,
                ModalEmphasis::Danger => ButtonStyle::Danger,
            };
            let face = ButtonFace::new(rect, a.label)
                .style(style)
                .radius(ACTION_H / 2.0)
                .text_size(body_size)
                .weight(TextWeight::Medium)
                .disabled(!a.enabled);
            // Measured against the fill this face actually resolved, so a
            // themed accent cannot leave the primary action unreadable.
            let (bg, _, _) = face.colors(theme);
            let face = face.with_label_color(Self::ink_on(bg, theme));
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
    }
}

#[cfg(test)]
mod tests {
    //! The geometry, and the two guarantees that made this widget exist:
    //! there is always a visible way out, and the primary action's label
    //! is readable on every palette the OS ships.

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

    /// The card is centred on the viewport, on both axes.
    #[test]
    fn card_centres_on_the_viewport() {
        let spec = ModalSpec::new("Workspace invite").body_height(120.0);
        let l = lay(&spec, &Theme::dark());
        let cx = (l.card.x0 + l.card.x1) / 2.0;
        let cy = (l.card.y0 + l.card.y1) / 2.0;
        assert!((cx - 800.0).abs() < 0.5, "card centre x = {cx}, viewport centre is 800");
        assert!((cy - 500.0).abs() < 0.5, "card centre y = {cy}, viewport centre is 500");
    }

    /// A closable modal always has a close control, and it is a real hit
    /// target rather than a decorative glyph.
    ///
    /// This is the defect the widget was built for: the invite card had
    /// no close control at all, and explained in a sentence that Escape
    /// was the way out.
    #[test]
    fn a_closable_modal_has_a_close_control_you_can_hit() {
        let spec = ModalSpec::new("Workspace invite").body_height(120.0);
        for (name, theme) in themes() {
            let l = lay(&spec, &theme);
            let close = l.close.unwrap_or_else(|| panic!("{name}: no close control"));
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
            assert!(l.dismisses(close.center()), "{name}: clicking close does not dismiss");
        }
    }

    /// The title never runs under the close control.
    #[test]
    fn the_title_band_stops_before_the_close_control() {
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

    /// Actions are given left-to-right and the last one lands at the
    /// right edge, which is where the primary action belongs.
    #[test]
    fn actions_are_left_to_right_with_the_last_at_the_right_edge() {
        let actions = [ModalAction::secondary("Cancel"), ModalAction::primary("Create invite")];
        let spec = ModalSpec::new("Workspace invite").actions(&actions).body_height(100.0);
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

    /// Every action is inside the card and hit-tests to its own index.
    #[test]
    fn every_action_is_inside_the_card_and_hit_tests_to_itself() {
        let actions = [
            ModalAction::secondary("Paste"),
            ModalAction::secondary("Cancel"),
            ModalAction::primary("Join"),
        ];
        let spec = ModalSpec::new("Join a workspace").actions(&actions).body_height(120.0);
        let l = lay(&spec, &Theme::dark());
        for (i, r) in l.actions.iter().enumerate() {
            assert!(r.area() > 0.0, "action {i} has no area");
            assert!(
                l.card.contains(r.center()),
                "action {i} at {r:?} is outside the card {:?}",
                l.card,
            );
            assert_eq!(l.hit(r.center()), Some(ModalHit::Action(i)));
            assert!(!l.dismisses(r.center()), "an action click must not dismiss on its own");
        }
    }

    /// A disabled action is not the default action.
    #[test]
    fn a_disabled_action_is_not_the_default_action() {
        let actions = [ModalAction::primary("Join").enabled(false)];
        let spec = ModalSpec::new("Join a workspace").actions(&actions).body_height(80.0);
        let l = lay(&spec, &Theme::dark());
        assert_eq!(l.primary, None, "a disabled action must not be what Enter fires");
    }

    /// The scrim covers the viewport, and a click on it dismisses only
    /// when the spec allows — but is never passed through either way.
    #[test]
    fn the_scrim_covers_everything_and_only_dismisses_when_asked() {
        let corner = Point::new(4.0, 4.0);

        let spec = ModalSpec::new("Workspace invite").body_height(100.0);
        let l = lay(&spec, &Theme::dark());
        assert_eq!(l.scrim, VIEWPORT, "the scrim must cover the whole viewport");
        assert_eq!(l.hit(corner), Some(ModalHit::Scrim));
        assert!(l.dismisses(corner), "the default modal dismisses on a scrim click");

        let sticky = ModalSpec::new("Workspace invite").body_height(100.0).dismiss_on_scrim(false);
        let l = lay(&sticky, &Theme::dark());
        assert_eq!(
            l.hit(corner),
            Some(ModalHit::Scrim),
            "a sticky modal still swallows the click — it just does not close",
        );
        assert!(!l.dismisses(corner));
    }

    /// A click inside the card, away from any control, is swallowed.
    #[test]
    fn the_card_swallows_clicks_that_hit_no_control() {
        let spec = ModalSpec::new("Workspace invite").body_height(160.0);
        let l = lay(&spec, &Theme::dark());
        let p = l.body.center();
        assert_eq!(l.hit(p), Some(ModalHit::Card));
        assert!(!l.dismisses(p), "clicking the body must never dismiss");
    }

    /// A body taller than the screen clamps the card to the viewport and
    /// says so, rather than drawing an action row off the bottom edge.
    #[test]
    fn an_oversized_body_clamps_the_card_and_reports_the_overflow() {
        let actions = [ModalAction::primary("OK")];
        let spec = ModalSpec::new("Tall").actions(&actions).body_height(9000.0);
        let l = lay(&spec, &Theme::dark());
        assert!(l.body_overflows, "a 9000px body in a 1000px viewport must report overflow");
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

    /// A body that fits does not claim to overflow.
    #[test]
    fn a_body_that_fits_does_not_report_overflow() {
        let spec = ModalSpec::new("Short").body_height(100.0);
        assert!(!lay(&spec, &Theme::dark()).body_overflows);
    }

    /// The card never touches an edge, even on an output smaller than the
    /// card's own maximum.
    #[test]
    fn the_card_keeps_its_margin_on_a_small_output() {
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

    /// The card is never wider than the maximum, however wide the output.
    #[test]
    fn the_card_stops_growing_at_its_maximum() {
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

    /// The modal's own ink clears AA against the card it is painted on,
    /// on every theme the OS ships.
    ///
    /// A previous card took a fixed ramp slot and was illegible on one of
    /// these palettes. This is that regression, pinned.
    #[test]
    fn modal_ink_is_legible_on_every_theme() {
        for (name, theme) in themes() {
            let card = ModalFrame::card_fill(&theme);
            let (ink, dim) = ModalFrame::ink(&theme);
            let r = contrast_ratio(ink, card);
            assert!(
                r >= MODAL_MIN_INK_CONTRAST,
                "{name}: modal ink contrast is {r:.2}:1 against the card — AA needs {MODAL_MIN_INK_CONTRAST}",
            );
            // The dim tier is a secondary tier, so it is held to the
            // large-text bar rather than the body bar.
            let rd = contrast_ratio(dim, card);
            assert!(rd >= 3.0, "{name}: dim ink contrast is {rd:.2}:1 — unreadable as a sub-label");
        }
    }

    /// Every action label is legible on the fill it is painted on, on
    /// every theme and in every weight.
    ///
    /// The hand-rolled invite card painted `text_primary` on `accent[5]`.
    /// This asserts the colour the modal *paints* — `ink_on` applied to
    /// the fill the shared face resolved — rather than a constant, so it
    /// keeps holding when the accent ramp is re-themed.
    #[test]
    fn every_action_label_is_legible_on_its_own_fill() {
        let styles = [
            (ModalEmphasis::Primary, ButtonStyle::Primary),
            (ModalEmphasis::Secondary, ButtonStyle::Neutral),
            (ModalEmphasis::Danger, ButtonStyle::Danger),
        ];
        for (name, theme) in themes() {
            for (emphasis, style) in styles {
                let face = ButtonFace::new(Rect::new(0.0, 0.0, 100.0, 34.0), "Create invite")
                    .style(style);
                let (bg, _border, _fg) = face.colors(&theme);
                let painted = ModalFrame::ink_on(bg, &theme);
                let r = contrast_ratio(painted, bg);
                assert!(
                    r >= 4.5,
                    "{name}/{emphasis:?}: label contrast is {r:.2}:1 on its fill — AA needs 4.5",
                );
                // And it is never worse than the chrome ink the
                // hand-rolled card reached for, which is the comparison
                // that says the measurement earned its keep.
                let naive = contrast_ratio(theme.text_primary, bg);
                assert!(
                    r >= naive,
                    "{name}/{emphasis:?}: measured label ({r:.2}:1) is worse than chrome ink ({naive:.2}:1)",
                );
            }
        }
    }

    /// `with_label_color` is actually honoured by the shared face — the
    /// override the legibility fix depends on.
    #[test]
    fn a_label_colour_override_reaches_the_resolved_face() {
        let theme = Theme::dark();
        let plain = ButtonFace::new(Rect::new(0.0, 0.0, 100.0, 34.0), "X").style(ButtonStyle::Primary);
        let (_, _, default_fg) = plain.colors(&theme);
        let forced = plain.with_label_color(Color::from_rgb8(1, 2, 3));
        let (_, _, fg) = forced.colors(&theme);
        assert_eq!(fg, Color::from_rgb8(1, 2, 3), "the override did not reach the face");
        assert_ne!(fg, default_fg, "the fixture must differ from the default to prove anything");
    }

    /// A modal with no actions has no action row, and its body takes the
    /// space back.
    #[test]
    fn a_modal_without_actions_gives_the_room_to_its_body() {
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

    /// A point outside the viewport hits nothing.
    #[test]
    fn a_point_off_the_viewport_hits_nothing() {
        let spec = ModalSpec::new("T").body_height(100.0);
        let l = lay(&spec, &Theme::dark());
        assert_eq!(l.hit(Point::new(-10.0, -10.0)), None);
    }
}
