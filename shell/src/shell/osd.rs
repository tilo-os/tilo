//! The transient on-screen display for volume, brightness and transport.
//!
//! A hardware key that changes something invisible (volume) or unreadable
//! (brightness, *because* it's what changed) needs an answer on screen: a
//! slab that appears, shows the new level, and gets out of the way — never
//! takes focus, never survives the moment it was about.
//!
//! Deliberately not a notification, which persists until acknowledged and
//! stacks with its peers. Pressing volume-up eight times must show one
//! slab counting up, not eight banners.

use std::time::{Duration, Instant};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};

/// How long the slab stays up after the last press. Long enough to read
/// after a single tap, short enough that a burst of presses does not
/// leave it hanging around afterwards.
pub const OSD_HOLD: Duration = Duration::from_millis(1_400);

/// The fade-out tail, inside [`OSD_HOLD`]'s trailing edge.
pub const OSD_FADE: Duration = Duration::from_millis(280);

/// What the slab is reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdKind {
    /// Audio output level. `muted` swaps the glyph and empties the bar —
    /// a muted sink at 60% is silent, and the bar should say so.
    Volume { muted: bool },
    /// Display backlight level.
    Brightness,
    /// Microphone capture mute. A toggle, not a level — the bar is
    /// omitted and the caption carries the state.
    MicMute { muted: bool },
    /// Media transport: the action taken, not a level.
    Transport(Transport),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Play,
    Pause,
    Next,
    Previous,
}

impl Transport {
    pub fn caption(self) -> &'static str {
        match self {
            Transport::Play => "Play",
            Transport::Pause => "Pause",
            Transport::Next => "Next track",
            Transport::Previous => "Previous track",
        }
    }
}

/// `level` is always clamped to `0.0..=1.0` by every `Osd` constructor,
/// so this never truncates or loses a sign — the result is always
/// `0..=100`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "level is clamped to 0.0..=1.0 by every constructor, so *100.0 is always 0..=100"
)]
fn level_to_percent(level: f32) -> u32 {
    (level * 100.0).round() as u32
}

/// One on-screen readout in flight.
#[derive(Clone, Copy, Debug)]
pub struct Osd {
    pub kind: OsdKind,
    /// `0.0..=1.0`. Ignored by the kinds that have no level.
    pub level: f32,
    /// When the value last changed; re-raising refreshes this rather than
    /// stacking a second slab.
    pub raised_at: Instant,
}

impl Osd {
    pub fn volume(level: f32, muted: bool) -> Self {
        Self {
            kind: OsdKind::Volume { muted },
            level: level.clamp(0.0, 1.0),
            raised_at: Instant::now(),
        }
    }

    pub fn brightness(level: f32) -> Self {
        Self {
            kind: OsdKind::Brightness,
            level: level.clamp(0.0, 1.0),
            raised_at: Instant::now(),
        }
    }

    pub fn mic_mute(muted: bool) -> Self {
        Self {
            kind: OsdKind::MicMute { muted },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn transport(t: Transport) -> Self {
        Self {
            kind: OsdKind::Transport(t),
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    /// Whether this kind draws a level bar — a bar pinned at zero next to
    /// "Next track" would read as a broken meter.
    pub fn has_level(&self) -> bool {
        matches!(self.kind, OsdKind::Volume { .. } | OsdKind::Brightness)
    }

    /// The one-line caption under the bar.
    pub fn caption(&self) -> String {
        match self.kind {
            OsdKind::Volume { muted: true } => "Muted".to_string(),
            OsdKind::Volume { muted: false } => {
                format!("Volume {}%", level_to_percent(self.level))
            }
            OsdKind::Brightness => format!("Brightness {}%", level_to_percent(self.level)),
            OsdKind::MicMute { muted: true } => "Microphone muted".to_string(),
            OsdKind::MicMute { muted: false } => "Microphone on".to_string(),
            OsdKind::Transport(t) => t.caption().to_string(),
        }
    }

    /// Opacity at `now`: opaque, then easing over [`OSD_FADE`], then `0.0`.
    pub fn alpha_at(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.raised_at);
        if elapsed >= OSD_HOLD {
            return 0.0;
        }
        // `elapsed < OSD_HOLD` is checked above, so this never actually
        // underflows; `unwrap_or` treats the (unreachable) alternative
        // as "already faded" rather than asserting the invariant.
        let Some(remaining) = OSD_HOLD.checked_sub(elapsed) else {
            return 0.0;
        };
        if remaining >= OSD_FADE {
            return 1.0;
        }
        let t = remaining.as_secs_f32() / OSD_FADE.as_secs_f32();
        // Ease-out so the last frames are the ones that thin out, rather
        // than a linear ramp that reads as a flicker at the end.
        t * t
    }

    /// Whether the slab should still be painted at `now`.
    pub fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.raised_at) < OSD_HOLD
    }

    /// Place the slab low and centred within `viewport`, away from the
    /// content most likely to be under the pointer.
    pub fn rect_in(viewport: Rect) -> Rect {
        let w = 260.0_f64.min(viewport.width() * 0.6);
        let h = 96.0_f64;
        let cx = viewport.center().x;
        let y1 = viewport.y1 - (viewport.height() * 0.12).max(56.0);
        Rect::new(cx - w / 2.0, y1 - h, cx + w / 2.0, y1)
    }

    /// Paint this readout inside `viewport` at the supplied lifecycle alpha.
    /// The OSD is intentionally non-interactive, so it owns no hit targets.
    pub fn draw(
        self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        alpha: f32,
    ) {
        if alpha <= 0.0 {
            return;
        }
        let rect = Self::rect_in(viewport);
        let faded = alpha < 0.999;
        if faded {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &viewport,
            );
        }

        draw_card(scene, transform, rect, theme);
        draw_glyph(scene, transform, rect, theme, self.kind);
        draw_level_bar(scene, transform, rect, theme, self);
        draw_caption(scene, transform, rect, theme, text_renderer, self);

        if faded {
            scene.pop_layer();
        }
    }
}

fn draw_card(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let colors = &theme.tokens.colors;
    let radius = 18.0_f64;
    // The slab floats over arbitrary app content, so it needs elevation to
    // remain distinct on both quiet and visually busy tiles.
    ui::surface::soft_shadow(scene, transform, rect, colors.neutral[0], 36.0, 14.0, 0.5);
    let body = RoundedRect::from_rect(rect, radius);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.overlay,
        None,
        &body,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.border_white_10,
        None,
        &body,
    );
}

fn draw_glyph(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme, kind: OsdKind) {
    let colors = &theme.tokens.colors;
    let glyph_px = 28.0_f64;
    let gx = rect.center().x;
    let gy = rect.y0 + 26.0;
    let glyph_rect = Rect::new(
        gx - glyph_px / 2.0,
        gy - glyph_px / 2.0,
        gx + glyph_px / 2.0,
        gy + glyph_px / 2.0,
    );
    let (icon, tint) = match kind {
        OsdKind::Volume { muted: true } => (ui::icons::Icon::VolumeX, colors.text_primary),
        OsdKind::Volume { muted: false } => (ui::icons::Icon::Volume2, colors.text_primary),
        OsdKind::Brightness => (ui::icons::Icon::Sun, colors.text_primary),
        OsdKind::MicMute { muted } => (
            ui::icons::Icon::MicOff,
            if muted {
                colors.semantic.warning
            } else {
                colors.text_primary
            },
        ),
        OsdKind::Transport(transport) => (
            match transport {
                Transport::Play => ui::icons::Icon::Play,
                Transport::Pause => ui::icons::Icon::Pause,
                Transport::Next => ui::icons::Icon::SkipForward,
                Transport::Previous => ui::icons::Icon::SkipBack,
            },
            colors.text_primary,
        ),
    };
    ui::icons::draw_icon(
        scene,
        transform,
        glyph_rect,
        icon,
        tint,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX * 1.4,
    );
}

fn draw_level_bar(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme, osd: Osd) {
    if !osd.has_level() {
        return;
    }
    let colors = &theme.tokens.colors;
    let bar_h = 6.0_f64;
    let inset = 24.0_f64;
    let bar = Rect::new(
        rect.x0 + inset,
        rect.y0 + 50.0,
        rect.x1 - inset,
        rect.y0 + 50.0 + bar_h,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surface_white_10,
        None,
        &RoundedRect::from_rect(bar, bar_h / 2.0),
    );
    // Muting keeps the track but empties its fill, so the slab does not
    // change shape while still agreeing with the crossed-out volume glyph.
    let filled = if matches!(osd.kind, OsdKind::Volume { muted: true }) {
        0.0
    } else {
        f64::from(osd.level.clamp(0.0, 1.0))
    };
    if filled > 0.0 {
        let w = (bar.width() * filled).max(bar_h);
        let fill_rect = Rect::new(bar.x0, bar.y0, bar.x0 + w, bar.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5],
            None,
            &RoundedRect::from_rect(fill_rect, bar_h / 2.0),
        );
    }
}

fn draw_caption(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    osd: Osd,
) {
    let colors = &theme.tokens.colors;
    let size = theme.tokens.typography.scale.sm;
    let caption = osd.caption();
    let (cw, ch) = text_renderer.measure_weighted(&caption, size, None, TextWeight::Medium);
    text_renderer.draw_text_weighted(
        scene,
        &caption,
        size,
        colors.text_dim,
        transform.pre_translate(
            (
                rect.center().x - f64::from(cw) / 2.0,
                rect.y1 - 12.0 - f64::from(ch),
            )
                .into(),
        ),
        None,
        TextWeight::Medium,
    );
}

/// Same key restarts the timer, different key replaces outright — either
/// way "never stack" lives in one place, not each caller.
pub fn raise(current: &mut Option<Osd>, next: Osd) {
    *current = Some(next);
}

/// Drop an expired slab. Returns `true` when something was cleared, so
/// the caller knows to bump its repaint revision.
pub fn expire(current: &mut Option<Osd>, now: Instant) -> bool {
    if current.is_some_and(|o| !o.is_live(now)) {
        *current = None;
        return true;
    }
    false
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact OSD animation endpoints and clamps are the behavior under test"
)]
mod tests {
    use super::*;

    #[test]
    fn a_muted_volume_reads_as_muted_not_as_a_percentage() {
        let o = Osd::volume(0.6, true);
        assert_eq!(o.caption(), "Muted");
        assert!(
            o.has_level(),
            "the bar is still drawn — empty — so the slab does not change shape on mute"
        );
    }

    #[test]
    fn levels_are_reported_as_whole_percent() {
        assert_eq!(Osd::volume(0.375, false).caption(), "Volume 38%");
        assert_eq!(Osd::brightness(0.0).caption(), "Brightness 0%");
        assert_eq!(Osd::brightness(1.0).caption(), "Brightness 100%");
    }

    #[test]
    fn transport_and_mic_have_no_level_bar() {
        assert!(!Osd::transport(Transport::Next).has_level());
        assert!(!Osd::mic_mute(true).has_level());
        assert_eq!(Osd::transport(Transport::Next).caption(), "Next track");
        assert_eq!(Osd::mic_mute(true).caption(), "Microphone muted");
    }

    #[test]
    fn a_level_out_of_range_is_clamped_rather_than_drawn_past_the_bar() {
        assert_eq!(Osd::volume(1.7, false).level, 1.0);
        assert_eq!(Osd::brightness(-0.2).level, 0.0);
    }

    #[test]
    fn a_fresh_slab_is_opaque_and_an_old_one_is_gone() {
        let o = Osd::volume(0.5, false);
        let t0 = o.raised_at;
        assert_eq!(o.alpha_at(t0), 1.0);
        assert!(o.is_live(t0));
        let past = t0 + OSD_HOLD;
        assert_eq!(o.alpha_at(past), 0.0);
        assert!(!o.is_live(past));
    }

    #[test]
    fn the_slab_thins_out_over_the_tail_rather_than_vanishing() {
        let o = Osd::brightness(0.5);
        let t0 = o.raised_at;
        // Just before the fade begins: still fully opaque.
        let before_fade = (t0 + OSD_HOLD)
            .checked_sub(OSD_FADE)
            .unwrap()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        assert_eq!(o.alpha_at(before_fade), 1.0);
        // Halfway through the fade: partly transparent, not yet gone.
        let mid = (t0 + OSD_HOLD).checked_sub(OSD_FADE / 2).unwrap();
        let a = o.alpha_at(mid);
        assert!(a > 0.0 && a < 1.0, "expected a partial alpha, got {a}");
    }

    #[test]
    fn a_second_press_restarts_the_slab_instead_of_stacking_one() {
        let mut cur = None;
        raise(&mut cur, Osd::volume(0.3, false));
        let first_raise = cur.unwrap().raised_at;
        std::thread::sleep(Duration::from_millis(2));
        raise(&mut cur, Osd::volume(0.4, false));
        let second = cur.unwrap();
        assert!(
            second.raised_at > first_raise,
            "the timer restarts on every press, so a burst shows one slab"
        );
        assert_eq!(second.level, 0.4, "and it shows the newest value");
    }

    #[test]
    fn a_different_key_replaces_rather_than_queues() {
        let mut cur = None;
        raise(&mut cur, Osd::volume(0.3, false));
        raise(&mut cur, Osd::brightness(0.9));
        assert_eq!(cur.unwrap().kind, OsdKind::Brightness);
    }

    #[test]
    fn expiry_reports_whether_it_changed_anything() {
        let mut cur = Some(Osd::volume(0.3, false));
        let t0 = cur.unwrap().raised_at;
        assert!(!expire(&mut cur, t0), "a live slab is not expired");
        assert!(expire(&mut cur, t0 + OSD_HOLD), "an old slab is dropped");
        assert!(cur.is_none());
        assert!(
            !expire(&mut cur, t0 + OSD_HOLD),
            "expiring nothing must not claim a repaint"
        );
    }

    #[test]
    fn slab_geometry_is_low_centered_and_width_bounded() {
        let viewport = Rect::new(20.0, 40.0, 1220.0, 840.0);
        let rect = Osd::rect_in(viewport);
        assert_eq!(rect.width(), 260.0);
        assert_eq!(rect.height(), 96.0);
        assert_eq!(rect.center().x, viewport.center().x);
        assert_eq!(viewport.y1 - rect.y1, 96.0);

        let narrow = Osd::rect_in(Rect::new(0.0, 0.0, 300.0, 500.0));
        assert_eq!(narrow.width(), 180.0);
    }
}
