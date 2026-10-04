//! A transient floating **status bubble**: a small floating Panel that
//! appears when a status changes (a page turn, a zoom step, a sync that
//! finished) or while the pointer rests on its corner, then fades away.
//!
//! Transient by contract: it is shown on change and hides on its own. What a
//! person needs to read at any time (the cursor's line and column, a word
//! count, conflict counts) is persistent status and belongs in
//! [`crate::chrome::status_bar`], which reserves its band. A capture never
//! forces the bubble open; only a screenshot scenario that stages the change
//! shows it.
//!
//! The bubble floats in the bottom-trailing corner of the area it reports
//! on, one gutter in, clear of the content's leading edge. It owns its text,
//! its timing and its paint; the app says what changed.

use crate::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::text_metrics::centered_text_top;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;

/// How long the bubble lingers after a poke (or after the pointer leaves its
/// zone) before it fades out, absent any new activity.
pub const DEFAULT_LINGER: Duration = Duration::from_millis(1900);
/// The tail of the linger spent fading to fully transparent, so the bubble
/// leaves quietly rather than blinking out.
const FADE: Duration = Duration::from_millis(300);

/// Every method takes an explicit `now: Instant`, so a test drives the
/// whole show → linger → fade → hide cycle without a wall clock.
#[derive(Clone, Debug)]
pub struct StatusBubble {
    linger: Duration,
    /// `None` while the pointer pins the bubble open.
    shown_at: Option<Instant>,
    hovered: bool,
    /// What the bubble says; [`Self::show`] pokes when it changes.
    text: String,
}

impl Default for StatusBubble {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBubble {
    pub fn new() -> Self {
        Self {
            linger: DEFAULT_LINGER,
            shown_at: None,
            hovered: false,
            text: String::new(),
        }
    }

    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Returns whether visibility changed (so the caller can request a redraw).
    pub fn poke(&mut self, now: Instant) -> bool {
        let was = self.is_visible(now);
        self.shown_at = Some(now);
        !was
    }

    /// While `true` the bubble is pinned fully open; on the transition to
    /// `false` it starts a fresh linger countdown.
    pub fn set_hover(&mut self, over: bool, now: Instant) -> bool {
        let was = self.is_visible(now);
        if over {
            self.hovered = true;
            self.shown_at = None;
        } else if self.hovered {
            self.hovered = false;
            self.shown_at = Some(now);
        }
        self.is_visible(now) != was
    }

    pub fn is_visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// Fully opaque while hovered or fresh, ramping to zero across the
    /// final `FADE` of the linger.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.hovered {
            return 1.0;
        }
        let Some(t) = self.shown_at else { return 0.0 };
        let e = now.saturating_duration_since(t);
        if e >= self.linger {
            return 0.0;
        }
        let Some(remain) = self.linger.checked_sub(e) else {
            // `e >= self.linger` already returned above, so this is
            // unreachable in practice; treat it as fully faded rather than
            // panic if that invariant ever slips.
            return 0.0;
        };
        if remain < FADE {
            (remain.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Report the status. A change shows the bubble (and restarts its
    /// linger); the same text again does nothing, so calling this every
    /// frame is fine. Returns whether visibility changed.
    pub fn show(&mut self, text: &str, now: Instant) -> bool {
        if self.text == text {
            return false;
        }
        text.clone_into(&mut self.text);
        self.poke(now)
    }

    /// What the bubble says.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The bubble's rect when it floats in `area`: the bottom-trailing
    /// corner, `spacing.s4` in.
    pub fn rect_in(&self, area: Rect, tr: &mut TextRenderer, theme: &Theme) -> Rect {
        let (w, h) = pill_size(&self.text, tr, theme, 0.0);
        let gutter = f64::from(theme.tokens.spacing.s4);
        let x1 = area.x1 - gutter;
        let y1 = area.y1 - gutter;
        Rect::new((x1 - w).max(area.x0), (y1 - h).max(area.y0), x1, y1)
    }

    /// The pointer moved to `p`: resting on the bubble's corner (the
    /// bubble plus a gutter around it) pins it open, leaving starts its
    /// linger. Returns whether visibility changed.
    pub fn pointer_moved(
        &mut self,
        area: Rect,
        p: Point,
        tr: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) -> bool {
        let gutter = f64::from(theme.tokens.spacing.s6);
        let zone = self.rect_in(area, tr, theme).inflate(gutter, gutter);
        self.set_hover(!self.text.is_empty() && zone.contains(p), now)
    }

    /// Paint the bubble in `area` at its current opacity; nothing while it
    /// is hidden. Returns the painted rect.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        now: Instant,
    ) -> Option<Rect> {
        let alpha = self.alpha(now);
        if alpha <= 0.0 || self.text.is_empty() {
            return None;
        }
        let rect = self.rect_in(area, tr, theme);
        let faded = alpha < 1.0;
        if faded {
            let reach = f64::from(theme.tokens.elevation.floating.blur) * 2.0;
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &rect.inflate(reach, reach),
            );
        }
        draw_pill(scene, transform, tr, theme, rect, &self.text);
        if faded {
            scene.pop_layer();
        }
        Some(rect)
    }

    /// Hovered/hidden states need no frames.
    pub fn needs_frame(&self, now: Instant) -> bool {
        !self.hovered
            && self
                .shown_at
                .is_some_and(|t| now.saturating_duration_since(t) < self.linger)
    }
}

/// The status pill's label: a `Label` in the theme.
fn pill_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Label)
}

/// The pill's size for `text`. `control` is a leading+trailing slot in px
/// each for a control inside the pill (`0.0` for a plain label).
pub fn pill_size(text: &str, tr: &mut TextRenderer, theme: &Theme, control: f64) -> (f64, f64) {
    let (tw, th) = tr.measure_style(text, None, &pill_style(theme));
    let pad_x = f64::from(theme.tokens.spacing.s3);
    let w = f64::from(tw) + 2.0 * pad_x + 2.0 * control;
    let h = pill_height(theme).max(f64::from(th) + 2.0 * pad_y(theme));
    (w, h)
}

/// Vertical padding of the pill's label.
fn pad_y(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1) * 1.5
}

/// Height shared by layout reservations and the rendered pill: the label's
/// line box plus the pill's vertical padding, never below a Small control.
pub fn pill_height(theme: &Theme) -> f64 {
    (f64::from(pill_style(theme).line_height_px()) + 2.0 * pad_y(theme))
        .max(f64::from(theme.tokens.metrics.bar_control_height))
}

/// Paint a bubble with `text` in `rect`: a floating Panel (the blurred
/// material, one soft shadow) with the label centred. For a bubble that
/// hosts controls beside its label; a plain status uses
/// [`StatusBubble::paint`].
pub fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    text: &str,
) {
    crate::widget::floating_surface::paint(scene, transform, rect, theme);
    let style = pill_style(theme);
    let ink = theme
        .on_ground(crate::chrome::panel_ground(theme))
        .tokens
        .colors
        .text_primary;
    let (tw, th) = tr.measure_style(text, None, &style);
    let tx = rect.x0 + (rect.width() - f64::from(tw)) / 2.0;
    // Centre the measured line box, not the font size.
    let ty = centered_text_top(rect.y0, rect.height(), f64::from(th));
    tr.draw_style(
        scene,
        text,
        ink,
        transform.pre_translate(Vec2::new(tx, ty)),
        None,
        &style,
    );
}

/// What a document's save state is, as the save status surface shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Nothing to report (memory only, or nothing changed yet).
    Idle,
    /// A save is in flight.
    Saving,
    /// The last change is stored.
    Saved,
    /// The last save failed; the text says why.
    Failed(String),
}

impl From<&crate::record::RecordSaveStatus> for SaveState {
    fn from(status: &crate::record::RecordSaveStatus) -> Self {
        use crate::record::RecordSaveStatus;
        match status {
            RecordSaveStatus::Saved => Self::Saved,
            RecordSaveStatus::Saving => Self::Saving,
            RecordSaveStatus::Failed(reason) => Self::Failed(reason.clone()),
            RecordSaveStatus::MemoryOnly => Self::Idle,
        }
    }
}

/// What a person did on the save status surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAction {
    /// "Try again" on a failed save.
    Retry,
    /// Dismissed the failure.
    Dismiss,
}

/// A document's save status, on the framework's status surfaces (HIG §7c):
/// "Saving…" and "Saved" are a change worth noticing once, so they show in
/// the transient [`StatusBubble`]; a failed save is an error the person must
/// act on, so it is an error toast (danger icon, the reason, a "Try again"
/// action and a close button) that stays until the save succeeds or it is
/// dismissed. An app reports the state and routes clicks; it never pins a
/// bubble open or paints its own save pill.
///
/// Both float in the bottom-trailing corner of the area they report on,
/// clear of its scroll bar gutter.
#[derive(Clone, Debug, Default)]
pub struct SaveStatusSurface {
    bubble: StatusBubble,
    state: Option<SaveState>,
    dismissed: bool,
    hit: crate::widget::toast::ToastCardHit,
    hover: Option<Point>,
}

impl SaveStatusSurface {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Report the save state. A change shows it; the same state again does
    /// nothing, so calling this every frame is fine. Returns whether what
    /// is shown changed.
    pub fn set(&mut self, state: SaveState, now: Instant) -> bool {
        if self.state.as_ref() == Some(&state) {
            return false;
        }
        let was_failed = self.failure().is_some();
        self.dismissed = false;
        let text = match &state {
            SaveState::Saving => crate::i18n!("save-status-saving"),
            SaveState::Saved => crate::i18n!("save-status-saved"),
            SaveState::Idle | SaveState::Failed(_) => String::new(),
        };
        self.state = Some(state);
        if !text.is_empty() {
            self.bubble.show(&text, now);
        }
        was_failed != self.failure().is_some() || !text.is_empty()
    }

    /// The reason of a failed save that is showing, if any.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            Some(SaveState::Failed(reason)) if !self.dismissed => Some(reason.as_str()),
            _ => None,
        }
    }

    /// Whether anything is showing at `now`.
    #[must_use]
    pub fn is_visible(&self, now: Instant) -> bool {
        self.failure().is_some() || self.bubble.is_visible(now)
    }

    /// Frames are needed only while the bubble lingers or fades.
    #[must_use]
    pub fn needs_frame(&self, now: Instant) -> bool {
        self.failure().is_none() && self.bubble.needs_frame(now)
    }

    /// The pointer moved to `p`; returns whether the hover changed.
    pub fn pointer_moved(&mut self, p: Point) -> bool {
        let before = self.hovered_action();
        self.hover = Some(p);
        before != self.hovered_action()
    }

    fn hovered_action(&self) -> Option<SaveAction> {
        let p = self.hover?;
        self.failure()?;
        if self.hit.primary_action_rect.contains(p) {
            Some(SaveAction::Retry)
        } else if self.hit.secondary_action_rect.contains(p) {
            Some(SaveAction::Dismiss)
        } else {
            None
        }
    }

    /// A click at `p`: the action it hits on a showing failure. Dismissing
    /// hides the failure until the state changes again.
    pub fn click(&mut self, p: Point) -> Option<SaveAction> {
        self.hover = Some(p);
        let action = self.hovered_action()?;
        if action == SaveAction::Dismiss {
            self.dismissed = true;
        }
        Some(action)
    }

    /// Whether `p` is on the failure card (so a host keeps the click).
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        self.failure().is_some() && self.hit.card_rect.contains(p)
    }

    /// The corner of `area` the surface floats in: `area` less its scroll
    /// bar gutter on the trailing edge.
    fn corner(area: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        Rect::new(area.x0, area.y0, (area.x1 - gutter).max(area.x0), area.y1)
    }

    /// The failure card in `corner`: the reason's two lines over the action
    /// row, a gutter in from the bottom-trailing corner.
    fn failure_rect(corner: Rect, theme: &Theme) -> Rect {
        let s = &theme.tokens.spacing;
        let gutter = f64::from(s.s4);
        let title = f64::from(theme.text(TextRole::Label).line_height_px());
        let body = f64::from(theme.text(TextRole::Caption).line_height_px());
        let actions = f64::from(theme.tokens.metrics.min_pointer_target);
        let height = f64::from(s.s2) * 2.0 + title + body + f64::from(s.s2) + actions;
        let width = f64::from(theme.tokens.metrics.panel_width_utility)
            .min((corner.width() - 2.0 * gutter).max(0.0));
        let x1 = corner.x1 - gutter;
        let y1 = corner.y1 - gutter;
        Rect::new(x1 - width, (y1 - height).max(corner.y0), x1, y1)
    }

    /// Paint what is showing in `area`; returns the painted rect.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        now: Instant,
    ) -> Option<Rect> {
        let corner = Self::corner(area, theme);
        let Some(reason) = self.failure().map(str::to_owned) else {
            self.hit = crate::widget::toast::ToastCardHit::default();
            return self.bubble.paint(scene, transform, tr, theme, corner, now);
        };
        let hovered = self.hovered_action();
        let title = crate::i18n!("save-status-failed");
        let retry = crate::i18n!("save-status-retry");
        let dismiss = crate::i18n!("save-status-dismiss");
        let card =
            crate::widget::toast::ToastCard::new(&title, crate::widget::toast::ToastKind::Error)
                .body(&reason)
                .actions(
                    &dismiss,
                    crate::icons::Icon::X,
                    hovered == Some(SaveAction::Dismiss),
                    &retry,
                    crate::icons::Icon::RotateCw,
                    hovered == Some(SaveAction::Retry),
                );
        let rect = Self::failure_rect(corner, theme);
        self.hit = crate::widget::toast::paint_card(scene, transform, tr, theme, rect, &card);
        if self.hit.secondary_action_rect.is_zero_area() {
            return None;
        }
        (!self.hit.card_rect.is_zero_area()).then_some(self.hit.card_rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bubble_geometry_reveal_linger_and_fade_follow_pokes_and_hover() {
        // Case `pill_reservation_matches_the_renderer_line_box_at_supported_scales`.
        {
            for scale in [1.0, 1.5, 2.0, 4.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, measured) = pill_size("Saved", &mut tr, &theme, 0.0);
                let reserved = pill_height(&theme);
                assert!(
                    (reserved - measured).abs() < 1e-3,
                    "layout reservation must equal draw geometry at {scale}x"
                );
            }
            let small = f64::from(Theme::dark().tokens.metrics.bar_control_height);
            assert!(pill_height(&Theme::dark()) >= small);
            assert!(pill_height(&Theme::dark().with_text_scale(2.0)) > pill_height(&Theme::dark()));
        }

        // Case `show_reveals_on_a_change_only_and_floats_in_the_trailing_corner`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            assert!(b.show("Saved", t0), "a new status shows the bubble");
            let later = t0 + Duration::from_millis(1500);
            assert!(!b.is_visible(later));
            assert!(!b.show("Saved", later), "the same status does not reshow");
            assert!(!b.is_visible(later));
            assert!(b.show("Synced", later), "a change does");
            let area = Rect::new(0.0, 0.0, 800.0, 600.0);
            let r = b.rect_in(area, &mut tr, &theme);
            let gutter = f64::from(theme.tokens.spacing.s4);
            assert!((r.x1 - (area.x1 - gutter)).abs() < 1e-9);
            assert!((r.y1 - (area.y1 - gutter)).abs() < 1e-9);
            let mut scene = Scene::new();
            assert_eq!(
                b.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, later),
                Some(r)
            );
            let hidden = later + Duration::from_secs(5);
            assert_eq!(
                b.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, hidden),
                None,
                "a faded bubble paints nothing"
            );
            assert!(b.pointer_moved(area, r.center(), &mut tr, &theme, hidden));
            assert!(
                b.is_visible(hidden + Duration::from_secs(60)),
                "hover pins it"
            );
        }

        // Case `poke_reveals_then_fades_after_linger`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            assert!(!b.is_visible(t0), "hidden until poked");
            assert!(b.poke(t0), "poke reveals and reports the change");
            assert!(b.is_visible(t0), "visible right after a poke");
            assert!((b.alpha(t0) - 1.0).abs() < 1e-6, "fully opaque when fresh");
            // Mid-linger, before the fade tail: still fully opaque.
            assert!((b.alpha(t0 + Duration::from_millis(500)) - 1.0).abs() < 1e-6);
            assert!(
                b.needs_frame(t0 + Duration::from_millis(500)),
                "counts down -> wants frames"
            );
            // Into the fade tail: partial opacity.
            let a = b.alpha(t0 + Duration::from_millis(850));
            assert!(a > 0.0 && a < 1.0, "fades out across the tail: {a}");
            // Past the linger: gone.
            assert!(!b.is_visible(t0 + Duration::from_secs(1)));
            assert!(!b.needs_frame(t0 + Duration::from_secs(1)));
        }

        // Case `hover_pins_open_then_lingers_after_leave`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.set_hover(true, t0);
            // Pinned open regardless of time while the pointer stays over it.
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "hover pins the bubble open"
            );
            assert!(
                !b.needs_frame(t0 + Duration::from_secs(60)),
                "a pinned bubble needs no frames"
            );
            // Leaving starts a fresh linger.
            b.set_hover(false, t0 + Duration::from_secs(60));
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "still up right after leaving"
            );
            assert!(
                !b.is_visible(t0 + Duration::from_secs(60) + Duration::from_secs(1)),
                "fades a linger after the pointer leaves"
            );
        }

        // Case `a_fresh_poke_restarts_the_countdown`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.poke(t0);
            // Almost expired…
            assert!(b.is_visible(t0 + Duration::from_millis(900)));
            // …then a new poke restarts the full linger.
            b.poke(t0 + Duration::from_millis(950));
            assert!(
                b.is_visible(t0 + Duration::from_millis(1800)),
                "the second poke extended it"
            );
            assert!(!b.is_visible(t0 + Duration::from_millis(1950)));
        }
    }

    /// Saving and saved are transient bubble text; a failure is an error
    /// card with "Try again" that stays until the save succeeds or it is
    /// dismissed, painted in danger and never as the bubble.
    #[test]
    fn a_failed_save_is_an_error_with_an_action_and_success_is_transient() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        assert!(surface.set(SaveState::Saved, t0));
        assert!(surface.failure().is_none() && surface.is_visible(t0));
        let later = t0 + DEFAULT_LINGER + Duration::from_millis(10);
        assert!(!surface.is_visible(later), "saved fades by itself");

        assert!(surface.set(SaveState::Failed("Disk full".into()), later));
        assert_eq!(surface.failure(), Some("Disk full"));
        let much_later = later + DEFAULT_LINGER * 4;
        assert!(surface.is_visible(much_later), "a failure stays");
        assert!(!surface.needs_frame(much_later), "and asks for no frames");
        let mut scene = Scene::new();
        let card = surface
            .paint(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                much_later,
            )
            .expect("the failure is painted");
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        assert!(
            card.x1 <= area.x1 - gutter,
            "clear of the scroll bar gutter"
        );
        let retry = surface.hit.primary_action_rect;
        assert!(!retry.is_zero_area(), "it offers Try again");
        assert_eq!(surface.click(retry.center()), Some(SaveAction::Retry));
        assert!(
            surface.failure().is_some(),
            "retrying keeps it until the state changes"
        );
        let dismiss = surface.hit.secondary_action_rect;
        assert_eq!(surface.click(dismiss.center()), Some(SaveAction::Dismiss));
        assert!(surface.failure().is_none(), "dismissed");
        assert!(surface.set(SaveState::Saving, much_later));
        assert!(surface.is_visible(much_later));
    }
}
