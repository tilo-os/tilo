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

/// Offscreen wgpu + vello PNG writer shared by the system-surface review
/// fixtures (OSD, power menu, lock screen, notifications) — surfaces the
/// ux-bench cannot open on macOS, photographed straight from their paint
/// paths instead. The same harness `overlay_host` uses for its fixtures.
#[cfg(test)]
#[path = "../../tests/support/offscreen.rs"]
pub(crate) mod review_offscreen;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
#[cfg(test)]
use vello::kurbo::Point;
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

    /// Whether the readout is in the short fade tail and therefore needs a
    /// frame on every presentation tick. The opaque part of the hold is
    /// deliberately excluded: a static OSD should not keep the compositor
    /// repainting for more than a second.
    pub fn needs_animation_frame_at(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.raised_at);
        elapsed >= OSD_HOLD.saturating_sub(OSD_FADE) && elapsed < OSD_HOLD
    }

    /// The next lifecycle boundary while the readout is static.
    ///
    /// During the opaque hold this wakes the compositor exactly when the
    /// fade begins. During the fade, continuous animation frames do the work,
    /// and the expiry boundary remains pending until the paint pass removes
    /// the readout. Returning that overdue boundary is intentional: the event
    /// loop decides whether a deadline caused damage by asking this method
    /// after waking, so dropping it at the deadline would skip the final
    /// clearing frame.
    pub fn next_wake_deadline_at(&self, now: Instant) -> Option<Instant> {
        let fade_start = self
            .raised_at
            .checked_add(OSD_HOLD.saturating_sub(OSD_FADE))
            .unwrap_or(self.raised_at);
        if now < fade_start {
            Some(fade_start)
        } else {
            self.raised_at.checked_add(OSD_HOLD)
        }
    }

    /// Place the slab at the visual centre of `viewport`. Hardware feedback
    /// belongs to the whole display rather than the app under the pointer;
    /// centring it also gives the same location at normal and compact sizes.
    pub fn rect_in(viewport: Rect) -> Rect {
        let w = 288.0_f64.min((viewport.width() - 32.0).max(0.0));
        let h = 104.0_f64.min((viewport.height() - 32.0).max(0.0));
        let center = viewport.center();
        Rect::new(
            center.x - w / 2.0,
            center.y - h / 2.0,
            center.x + w / 2.0,
            center.y + h / 2.0,
        )
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
        // Theme type tokens already carry the user's text scale, and the
        // slab paints with those resolved sizes; the renderer must not scale
        // them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restored at the end.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
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

        let caption = self.caption();
        let (_, caption_h) = text_renderer.measure_weighted(
            &caption,
            theme.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        let stack = OsdStack::layout(rect, self.has_level(), f64::from(caption_h), theme);
        draw_card(scene, transform, rect, theme);
        draw_glyph(scene, transform, stack.glyph, theme, self.kind);
        if let Some(bar) = stack.bar {
            draw_level_bar(scene, transform, bar, theme, self);
        }
        draw_caption(
            scene,
            transform,
            stack.caption,
            theme,
            text_renderer,
            &caption,
        );

        if faded {
            scene.pop_layer();
        }
        text_renderer.set_text_scale(renderer_scale);
    }
}

/// The slab's vertical rhythm: glyph, optional level bar and caption as
/// one stack centred in the card with `spacing.s3` between rows. Keeping
/// layout separate from paint makes the rhythm testable and means a
/// slab without a level bar (mic, transport) is centred rather than
/// top-heavy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OsdStack {
    pub glyph: Rect,
    pub bar: Option<Rect>,
    pub caption: Rect,
}

impl OsdStack {
    pub(crate) const GLYPH_PX: f64 = 28.0;
    const BAR_H: f64 = 6.0;

    pub(crate) fn layout(rect: Rect, has_level: bool, caption_h: f64, theme: &Theme) -> Self {
        let gap = f64::from(theme.tokens.spacing.s3);
        let inset = f64::from(theme.tokens.spacing.s6);
        let bar_block = if has_level { Self::BAR_H + gap } else { 0.0 };
        let stack_h = Self::GLYPH_PX + gap + bar_block + caption_h;
        let top = rect.y0 + ((rect.height() - stack_h) / 2.0).max(0.0);
        let cx = rect.center().x;
        let glyph = Rect::new(
            cx - Self::GLYPH_PX / 2.0,
            top,
            cx + Self::GLYPH_PX / 2.0,
            top + Self::GLYPH_PX,
        );
        let mut y = glyph.y1 + gap;
        let bar = has_level.then(|| {
            let bar = Rect::new(rect.x0 + inset, y, rect.x1 - inset, y + Self::BAR_H);
            y = bar.y1 + gap;
            bar
        });
        let caption = Rect::new(rect.x0 + inset, y, rect.x1 - inset, y + caption_h);
        Self {
            glyph,
            bar,
            caption,
        }
    }
}

fn draw_card(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    // Panel role (HIG §9.1/§9.2): radius, elevation, ground and edge come
    // from the same helpers every other floating Panel uses, so the slab is
    // the same material — `panel_ground` walks the overlay tier away from
    // the desktop where a theme's declared overlay would be swallowed.
    let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
    ui::chrome::shell::panel_shadow(scene, transform, rect, radius, theme, 1.0);
    let body = RoundedRect::from_rect(rect, radius);
    scene.fill(
        Fill::NonZero,
        transform,
        ui::chrome::shell::panel_ground(theme),
        None,
        &body,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::shell::panel_border(theme),
        None,
        &body,
    );
}

fn draw_glyph(
    scene: &mut Scene,
    transform: Affine,
    glyph_rect: Rect,
    theme: &Theme,
    kind: OsdKind,
) {
    let colors = &theme.tokens.colors;
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

fn draw_level_bar(scene: &mut Scene, transform: Affine, bar: Rect, theme: &Theme, osd: Osd) {
    let colors = &theme.tokens.colors;
    let bar_h = bar.height();
    scene.fill(
        Fill::NonZero,
        transform,
        colors.lift(0.72),
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
    caption_rect: Rect,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    caption: &str,
) {
    let colors = &theme.tokens.colors;
    // Control label role (HIG §9.6); the pass has reset the renderer, so
    // the theme-resolved size is painted exactly once.
    let size = theme.tokens.typography.scale.sm;
    let (cw, _) = text_renderer.measure_weighted(caption, size, None, TextWeight::Medium);
    text_renderer.draw_text_weighted(
        scene,
        caption,
        size,
        colors.text_dim,
        transform.pre_translate(
            (
                caption_rect.center().x - f64::from(cw) / 2.0,
                caption_rect.y0,
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
    fn opaque_hold_sleeps_until_fade_then_only_fade_animates() {
        let o = Osd::volume(0.5, false);
        let t0 = o.raised_at;
        let expiry = t0.checked_add(OSD_HOLD).unwrap();
        let fade_start = expiry.checked_sub(OSD_FADE).unwrap();

        assert!(!o.needs_animation_frame_at(t0));
        assert_eq!(o.next_wake_deadline_at(t0), Some(fade_start));
        assert!(o.needs_animation_frame_at(fade_start));
        assert_eq!(o.next_wake_deadline_at(fade_start), Some(expiry));
        assert!(o.needs_animation_frame_at(expiry.checked_sub(Duration::from_millis(1)).unwrap()));
        assert!(!o.needs_animation_frame_at(expiry));
        assert_eq!(
            o.next_wake_deadline_at(expiry),
            Some(expiry),
            "the overdue expiry remains observable until the paint pass clears it"
        );
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
    fn slab_stack_is_centred_with_token_gaps_with_and_without_a_level_bar() {
        let theme = Theme::dark();
        let rect = Osd::rect_in(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let gap = f64::from(theme.tokens.spacing.s3);
        let caption_h = 15.0;

        let with = OsdStack::layout(rect, true, caption_h, &theme);
        let bar = with.bar.expect("a level slab has a bar");
        assert_eq!(bar.y0 - with.glyph.y1, gap);
        assert_eq!(with.caption.y0 - bar.y1, gap);
        assert_eq!(with.glyph.center().x, rect.center().x);
        assert!(
            ((with.glyph.y0 - rect.y0) - (rect.y1 - with.caption.y1)).abs() < 1e-9,
            "the stack is vertically centred in the slab"
        );
        assert!(with.glyph.y0 >= rect.y0 && with.caption.y1 <= rect.y1);

        let without = OsdStack::layout(rect, false, caption_h, &theme);
        assert!(without.bar.is_none());
        assert_eq!(without.caption.y0 - without.glyph.y1, gap);
        assert!(
            ((without.glyph.y0 - rect.y0) - (rect.y1 - without.caption.y1)).abs() < 1e-9,
            "a slab without a bar is centred too, not top-heavy"
        );
        assert!(without.glyph.y0 > with.glyph.y0);
    }

    #[test]
    fn slab_geometry_is_low_centered_and_width_bounded() {
        let viewport = Rect::new(20.0, 40.0, 1220.0, 840.0);
        let rect = Osd::rect_in(viewport);
        assert_eq!(rect.width(), 288.0);
        assert_eq!(rect.height(), 104.0);
        assert_eq!(rect.center(), viewport.center());

        let narrow = Osd::rect_in(Rect::new(0.0, 0.0, 300.0, 500.0));
        assert_eq!(narrow.width(), 268.0);
        assert_eq!(narrow.height(), 104.0);
        assert_eq!(narrow.center(), vello::kurbo::Point::new(150.0, 250.0));
        assert_eq!(narrow.x0, 16.0);

        let tiny = Osd::rect_in(Rect::new(5.0, 7.0, 125.0, 87.0));
        assert_eq!(tiny.width(), 88.0);
        assert_eq!(tiny.height(), 48.0);
        assert_eq!(tiny.center(), vello::kurbo::Point::new(65.0, 47.0));
    }

    /// HIG §7a: the caption is scaled exactly once. The pass paints the
    /// theme-resolved `sm` with the renderer reset to 1×, so at 200 % the
    /// line is twice the 1× line — not four times — and the renderer's own
    /// scale is handed back to the widgets that follow.
    #[test]
    fn caption_type_is_scaled_once_at_large_text() {
        let base = Theme::dark();
        let large = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let viewport = Rect::new(0.0, 0.0, 640.0, 360.0);
        let mut scene = Scene::new();
        Osd::brightness(0.5).draw(&mut scene, Affine::IDENTITY, &mut tr, &large, viewport, 1.0);
        assert_eq!(
            tr.text_scale(),
            2.0,
            "the pass restores the renderer's scale"
        );
        // What the pass painted with: the theme's `sm` on a 1× renderer.
        let caption = Osd::brightness(0.5).caption();
        let mut at_pass = TextRenderer::new();
        let (_, h1) = at_pass.measure_weighted(
            &caption,
            base.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        let (_, h2) = at_pass.measure_weighted(
            &caption,
            large.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        assert!(
            (h2 - 2.0 * h1).abs() < 1.0,
            "200 % caption line {h2} should be twice the 1× line {h1}"
        );
    }

    /// Review fixtures: the ux-bench cannot raise an OSD on macOS, so the
    /// slab is photographed straight from its paint path — each kind, at 1×
    /// and 200 % text, on the dark, light and high-contrast palettes. The
    /// rendered pixels are asserted, not just the file: the slab's interior
    /// is the Panel ground, whatever the palette.
    #[test]
    fn osd_review_fixtures_are_written() {
        let out =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-review/s2-r2/osd");
        let (w, h) = (640_u32, 360_u32);
        let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        let mut renderer = review_offscreen::OffscreenRenderer::new(w, h);
        let variants = [
            ("dark-1x", Theme::dark(), 1.0_f32),
            ("dark-2x", Theme::dark().with_text_scale(2.0), 2.0),
            ("light-1x", Theme::light(), 1.0),
            ("high-contrast-1x", Theme::high_contrast(), 1.0),
        ];
        for (name, theme, scale) in variants {
            for (kind, osd) in [
                ("volume", Osd::volume(0.62, false)),
                ("mic-muted", Osd::mic_mute(true)),
                ("transport", Osd::transport(Transport::Next)),
            ] {
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                let mut scene = Scene::new();
                osd.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    viewport,
                    1.0,
                );
                let path = out.join(format!("{name}-{kind}.png"));
                let ground = theme.tokens.colors.surfaces.base;
                renderer.render_png(&scene, ground, &path);
                assert!(path.is_file(), "OSD fixture missing: {}", path.display());
                let rgba = renderer.render_resilient(&scene, ground);
                let slab = Osd::rect_in(viewport);
                let probe = Point::new(slab.x0 + 12.0, slab.center().y);
                assert_pixel_is(
                    &rgba,
                    w,
                    probe,
                    ui::chrome::shell::panel_ground(&theme),
                    &format!("{name}-{kind}: slab interior is the Panel ground"),
                );
            }
        }
    }

    /// The rendered pixel at `at` is `expected` to within rounding.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe points are inside a positive viewport"
    )]
    fn assert_pixel_is(
        rgba: &[u8],
        width: u32,
        at: Point,
        expected: vello::peniko::Color,
        what: &str,
    ) {
        let (r, g, b, _) =
            review_offscreen::OffscreenRenderer::sample(rgba, width, at.x as u32, at.y as u32);
        let e = expected.to_rgba8();
        let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
        assert!(
            close(r, e.r) && close(g, e.g) && close(b, e.b),
            "{what}: got ({r},{g},{b}) expected ({},{},{})",
            e.r,
            e.g,
            e.b
        );
    }

    /// HIG §7a: the caption is scaled exactly once. The pass paints the
    /// theme-resolved `sm` with the renderer reset to 1×, so at 200 % the
    /// line is twice the 1× line — not four times — and the renderer's own
    /// scale is handed back to the widgets that follow.
    #[test]
    fn caption_type_is_scaled_once_at_large_text() {
        let base = Theme::dark();
        let large = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let viewport = Rect::new(0.0, 0.0, 640.0, 360.0);
        let mut scene = Scene::new();
        Osd::brightness(0.5).draw(&mut scene, Affine::IDENTITY, &mut tr, &large, viewport, 1.0);
        assert_eq!(
            tr.text_scale(),
            2.0,
            "the pass restores the renderer's scale"
        );
        // What the pass painted with: the theme's `sm` on a 1× renderer.
        let caption = Osd::brightness(0.5).caption();
        let mut at_pass = TextRenderer::new();
        let (_, h1) = at_pass.measure_weighted(
            &caption,
            base.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        let (_, h2) = at_pass.measure_weighted(
            &caption,
            large.tokens.typography.scale.sm,
            None,
            TextWeight::Medium,
        );
        assert!(
            (h2 - 2.0 * h1).abs() < 1.0,
            "200 % caption line {h2} should be twice the 1× line {h1}"
        );
    }

    /// Review fixtures: the ux-bench cannot raise an OSD on macOS, so the
    /// slab is photographed straight from its paint path — each kind, at 1×
    /// and 200 % text, on the dark, light and high-contrast palettes. The
    /// rendered pixels are asserted, not just the file: the slab's interior
    /// is the Panel ground, whatever the palette.
    #[test]
    fn osd_review_fixtures_are_written() {
        let out =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-review/s2-r2/osd");
        let (w, h) = (640_u32, 360_u32);
        let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        let mut renderer = review_offscreen::OffscreenRenderer::new(w, h);
        let variants = [
            ("dark-1x", Theme::dark(), 1.0_f32),
            ("dark-2x", Theme::dark().with_text_scale(2.0), 2.0),
            ("light-1x", Theme::light(), 1.0),
            ("high-contrast-1x", Theme::high_contrast(), 1.0),
        ];
        for (name, theme, scale) in variants {
            for (kind, osd) in [
                ("volume", Osd::volume(0.62, false)),
                ("mic-muted", Osd::mic_mute(true)),
                ("transport", Osd::transport(Transport::Next)),
            ] {
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                let mut scene = Scene::new();
                osd.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    viewport,
                    1.0,
                );
                let path = out.join(format!("{name}-{kind}.png"));
                let ground = theme.tokens.colors.surfaces.base;
                renderer.render_png(&scene, ground, &path);
                assert!(path.is_file(), "OSD fixture missing: {}", path.display());
                let rgba = renderer.render_resilient(&scene, ground);
                let slab = Osd::rect_in(viewport);
                let probe = Point::new(slab.x0 + 12.0, slab.center().y);
                assert_pixel_is(
                    &rgba,
                    w,
                    probe,
                    ui::chrome::shell::panel_ground(&theme),
                    &format!("{name}-{kind}: slab interior is the Panel ground"),
                );
            }
        }
    }

    /// The rendered pixel at `at` is `expected` to within rounding.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe points are inside a positive viewport"
    )]
    fn assert_pixel_is(
        rgba: &[u8],
        width: u32,
        at: Point,
        expected: vello::peniko::Color,
        what: &str,
    ) {
        let (r, g, b, _) =
            review_offscreen::OffscreenRenderer::sample(rgba, width, at.x as u32, at.y as u32);
        let e = expected.to_rgba8();
        let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
        assert!(
            close(r, e.r) && close(g, e.g) && close(b, e.b),
            "{what}: got ({r},{g},{b}) expected ({},{},{})",
            e.r,
            e.g,
            e.b
        );
    }
}
