//! The transient on-screen display for the hardware keys: volume,
//! brightness, transport and the machine toggles.
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

use crate::shell::i18n::{t, t_args};

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
    /// Keyboard backlight level.
    KeyboardBacklight,
    /// A brightness key on a machine with no backlight the session can
    /// drive — an external monitor, a desktop, a VM. Saying so beats a key
    /// that silently does nothing.
    NoBacklight,
    /// Airplane mode after the wireless key toggled it.
    AirplaneMode { on: bool },
    /// The touchpad after its toggle key.
    Touchpad { enabled: bool },
    /// A client locked or confined the pointer (`held`), or it was just
    /// released. The caption names the way out.
    PointerHeld { held: bool },
    /// Mouse keys after its Shift+Alt+Num Lock chord.
    MouseKeys { enabled: bool },
    /// The magnifier after a magnifier shortcut; `zoom` is the factor, `1` meaning off.
    Magnifier { zoom: u8 },
    /// The keyboard input source just switched to.
    InputSource(OsdLabel),
    /// The focused app takes every key, shell shortcuts included
    /// (`captured`), or the shortcuts were just handed back.
    KeysCaptured { captured: bool },
    /// A tablet pad's mode button switched its rings, strips and buttons
    /// to `mode` (from zero) of `modes`.
    PadMode { mode: u32, modes: u32 },
}

/// A short caption carried inline so a readout stays `Copy`: the slab is
/// raised from input handlers every keypress and must never allocate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OsdLabel {
    bytes: [u8; Self::CAPACITY],
    len: u8,
}

impl OsdLabel {
    const CAPACITY: usize = 48;

    /// `text`, cut at the last whole character that fits.
    pub fn new(text: &str) -> Self {
        let mut end = text.len().min(Self::CAPACITY);
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let mut bytes = [0; Self::CAPACITY];
        if let (Some(dst), Some(src)) = (bytes.get_mut(..end), text.as_bytes().get(..end)) {
            dst.copy_from_slice(src);
        }
        Self {
            bytes,
            len: u8::try_from(end).unwrap_or(0),
        }
    }

    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }
}

impl std::fmt::Debug for OsdLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OsdLabel").field(&self.as_str()).finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

impl Transport {
    pub fn caption(self) -> String {
        match self {
            Transport::Play => t("osd-play"),
            Transport::Pause => t("osd-pause"),
            Transport::Next => t("osd-next-track"),
            Transport::Previous => t("osd-previous-track"),
            Transport::Stop => t("osd-stop"),
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

    pub fn pointer_held(held: bool) -> Self {
        Self {
            kind: OsdKind::PointerHeld { held },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn keys_captured(captured: bool) -> Self {
        Self {
            kind: OsdKind::KeysCaptured { captured },
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

    pub fn keyboard_backlight(level: f32) -> Self {
        Self {
            kind: OsdKind::KeyboardBacklight,
            level: level.clamp(0.0, 1.0),
            raised_at: Instant::now(),
        }
    }

    pub fn no_backlight() -> Self {
        Self {
            kind: OsdKind::NoBacklight,
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn airplane_mode(on: bool) -> Self {
        Self {
            kind: OsdKind::AirplaneMode { on },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn touchpad(enabled: bool) -> Self {
        Self {
            kind: OsdKind::Touchpad { enabled },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn mouse_keys(enabled: bool) -> Self {
        Self {
            kind: OsdKind::MouseKeys { enabled },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    /// The magnifier at `zoom`; the bar fills from off to the strongest zoom.
    pub fn magnifier(zoom: u8) -> Self {
        use settings_store::magnifier::{MAX_ZOOM, OFF};
        let zoom = zoom.clamp(OFF, MAX_ZOOM);
        Self {
            kind: OsdKind::Magnifier { zoom },
            level: f32::from(zoom.saturating_sub(OFF)) / f32::from(MAX_ZOOM.saturating_sub(OFF)),
            raised_at: Instant::now(),
        }
    }

    /// A tablet pad now in `mode` (from zero) of `modes`.
    pub fn pad_mode(mode: u32, modes: u32) -> Self {
        Self {
            kind: OsdKind::PadMode { mode, modes },
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    pub fn input_source(name: &str) -> Self {
        Self {
            kind: OsdKind::InputSource(OsdLabel::new(name)),
            level: 0.0,
            raised_at: Instant::now(),
        }
    }

    /// Whether this kind draws a level bar — a bar pinned at zero next to
    /// "Next track" would read as a broken meter.
    pub fn has_level(&self) -> bool {
        matches!(
            self.kind,
            OsdKind::Volume { .. }
                | OsdKind::Brightness
                | OsdKind::KeyboardBacklight
                | OsdKind::Magnifier { .. }
        )
    }

    /// The one-line caption under the bar.
    pub fn caption(&self) -> String {
        match self.kind {
            OsdKind::Volume { muted: true } => t("osd-muted"),
            OsdKind::Volume { muted: false } => t_args(
                "osd-volume",
                &[("percent", level_to_percent(self.level).into())],
            ),
            OsdKind::Brightness => t_args(
                "osd-brightness",
                &[("percent", level_to_percent(self.level).into())],
            ),
            OsdKind::MicMute { muted: true } => t("osd-microphone-muted"),
            OsdKind::MicMute { muted: false } => t("osd-microphone-on"),
            OsdKind::Transport(transport) => transport.caption(),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-keyboard-backlight-off"),
            OsdKind::KeyboardBacklight => t_args(
                "osd-keyboard-backlight",
                &[("percent", level_to_percent(self.level).into())],
            ),
            OsdKind::NoBacklight => t("osd-no-backlight"),
            OsdKind::AirplaneMode { on: true } => t("osd-airplane-mode-on"),
            OsdKind::AirplaneMode { on: false } => t("osd-airplane-mode-off"),
            OsdKind::Touchpad { enabled: true } => t("osd-touchpad-on"),
            OsdKind::Magnifier { zoom } if zoom <= settings_store::magnifier::OFF => {
                t("osd-magnifier-off")
            }
            OsdKind::Magnifier { zoom } => t_args(
                "osd-magnifier",
                &[("zoom", settings_store::magnifier::zoom_label(zoom).into())],
            ),
            OsdKind::Touchpad { enabled: false } => t("osd-touchpad-off"),
            OsdKind::PointerHeld { held: true } => t_args(
                "osd-pointer-held",
                &[("chord", crate::compositor::release_chord_label().into())],
            ),
            OsdKind::PointerHeld { held: false } => t("osd-pointer-released"),
            OsdKind::MouseKeys { enabled: true } => t("osd-mouse-keys-on"),
            OsdKind::MouseKeys { enabled: false } => t("osd-mouse-keys-off"),
            OsdKind::InputSource(name) => name.as_str().to_string(),
            OsdKind::KeysCaptured { captured: true } => t_args(
                "osd-keys-captured",
                &[("chord", crate::compositor::release_chord_label().into())],
            ),
            OsdKind::KeysCaptured { captured: false } => t("osd-keys-released"),
            OsdKind::PadMode { mode, modes } => t_args(
                "osd-pad-mode",
                &[
                    ("mode", mode.saturating_add(1).into()),
                    ("modes", modes.max(mode.saturating_add(1)).into()),
                ],
            ),
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
                Transport::Stop => ui::icons::Icon::Square,
            },
            colors.text_primary,
        ),
        OsdKind::KeyboardBacklight | OsdKind::InputSource(_) | OsdKind::KeysCaptured { .. } => {
            (ui::icons::Icon::Keyboard, colors.text_primary)
        }
        OsdKind::NoBacklight => (ui::icons::Icon::Monitor, colors.text_dim),
        OsdKind::AirplaneMode { on: true } => (ui::icons::Icon::WifiOff, colors.text_primary),
        OsdKind::AirplaneMode { on: false } => (ui::icons::Icon::Wifi, colors.text_primary),
        OsdKind::Touchpad { enabled } | OsdKind::MouseKeys { enabled } => (
            ui::icons::Icon::Mouse,
            if enabled {
                colors.text_primary
            } else {
                colors.text_dim
            },
        ),
        OsdKind::PointerHeld { .. } => (ui::icons::Icon::MousePointer2, colors.text_primary),
        OsdKind::PadMode { .. } => (ui::icons::Icon::PenTool, colors.text_primary),
        OsdKind::Magnifier { zoom } => (
            ui::icons::Icon::Search,
            if zoom > settings_store::magnifier::OFF {
                colors.text_primary
            } else {
                colors.text_dim
            },
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
            colors.intent.inverted,
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
    fn osd_levels_read_as_whole_clamped_percent_or_muted() {
        fn a_muted_volume_reads_as_muted_not_as_a_percentage() {
            let o = Osd::volume(0.6, true);
            assert_eq!(o.caption(), "Muted");
            assert!(
                o.has_level(),
                "the bar is still drawn — empty — so the slab does not change shape on mute"
            );
        }

        fn levels_are_reported_as_whole_percent() {
            assert_eq!(Osd::volume(0.375, false).caption(), "Volume 38%");
            assert_eq!(Osd::brightness(0.0).caption(), "Brightness 0%");
            assert_eq!(Osd::brightness(1.0).caption(), "Brightness 100%");
        }

        fn transport_and_mic_have_no_level_bar() {
            assert!(!Osd::transport(Transport::Next).has_level());
            assert!(!Osd::mic_mute(true).has_level());
            assert_eq!(Osd::transport(Transport::Next).caption(), "Next track");
            assert_eq!(Osd::mic_mute(true).caption(), "Microphone muted");
        }

        fn machine_toggles_say_what_they_left_behind() {
            assert_eq!(Osd::airplane_mode(true).caption(), "Airplane mode on");
            assert_eq!(Osd::airplane_mode(false).caption(), "Airplane mode off");
            assert_eq!(Osd::touchpad(false).caption(), "Touchpad off");
            assert_eq!(Osd::touchpad(true).caption(), "Touchpad on");
            assert_eq!(Osd::mouse_keys(true).caption(), "Mouse keys on");
            assert_eq!(Osd::mouse_keys(false).caption(), "Mouse keys off");
            assert_eq!(Osd::no_backlight().caption(), "No adjustable display");
            assert_eq!(Osd::transport(Transport::Stop).caption(), "Stop");
            for toggle in [
                Osd::airplane_mode(true),
                Osd::touchpad(false),
                Osd::no_backlight(),
            ] {
                assert!(
                    !toggle.has_level(),
                    "{:?} is a state, not a level",
                    toggle.kind
                );
            }
        }

        fn keyboard_backlight_reads_as_a_level_and_off_at_zero() {
            let lit = Osd::keyboard_backlight(0.5);
            assert!(lit.has_level());
            assert_eq!(lit.caption(), "Keyboard backlight 50%");
            assert_eq!(
                Osd::keyboard_backlight(0.0).caption(),
                "Keyboard backlight off"
            );
        }

        fn a_level_out_of_range_is_clamped_rather_than_drawn_past_the_bar() {
            assert_eq!(Osd::volume(1.7, false).level, 1.0);
            assert_eq!(Osd::brightness(-0.2).level, 0.0);
        }

        a_muted_volume_reads_as_muted_not_as_a_percentage();
        levels_are_reported_as_whole_percent();
        transport_and_mic_have_no_level_bar();
        a_level_out_of_range_is_clamped_rather_than_drawn_past_the_bar();
        machine_toggles_say_what_they_left_behind();
        keyboard_backlight_reads_as_a_level_and_off_at_zero();
    }

    #[test]
    fn osd_slab_fades_over_its_tail_and_is_replaced_not_stacked() {
        fn a_fresh_slab_is_opaque_and_an_old_one_is_gone() {
            let o = Osd::volume(0.5, false);
            let t0 = o.raised_at;
            assert_eq!(o.alpha_at(t0), 1.0);
            assert!(o.is_live(t0));
            let past = t0 + OSD_HOLD;
            assert_eq!(o.alpha_at(past), 0.0);
            assert!(!o.is_live(past));
        }

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

        fn opaque_hold_sleeps_until_fade_then_only_fade_animates() {
            let o = Osd::volume(0.5, false);
            let t0 = o.raised_at;
            let expiry = t0.checked_add(OSD_HOLD).unwrap();
            let fade_start = expiry.checked_sub(OSD_FADE).unwrap();

            assert!(!o.needs_animation_frame_at(t0));
            assert_eq!(o.next_wake_deadline_at(t0), Some(fade_start));
            assert!(o.needs_animation_frame_at(fade_start));
            assert_eq!(o.next_wake_deadline_at(fade_start), Some(expiry));
            assert!(
                o.needs_animation_frame_at(expiry.checked_sub(Duration::from_millis(1)).unwrap())
            );
            assert!(!o.needs_animation_frame_at(expiry));
            assert_eq!(
                o.next_wake_deadline_at(expiry),
                Some(expiry),
                "the overdue expiry remains observable until the paint pass clears it"
            );
        }

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

        fn a_different_key_replaces_rather_than_queues() {
            let mut cur = None;
            raise(&mut cur, Osd::volume(0.3, false));
            raise(&mut cur, Osd::brightness(0.9));
            assert_eq!(cur.unwrap().kind, OsdKind::Brightness);
        }

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

        a_fresh_slab_is_opaque_and_an_old_one_is_gone();
        the_slab_thins_out_over_the_tail_rather_than_vanishing();
        opaque_hold_sleeps_until_fade_then_only_fade_animates();
        a_second_press_restarts_the_slab_instead_of_stacking_one();
        a_different_key_replaces_rather_than_queues();
        expiry_reports_whether_it_changed_anything();
    }

    #[test]
    fn an_input_source_readout_names_the_layout_and_stays_bounded() {
        let osd = Osd::input_source("German (no dead keys)");
        assert_eq!(osd.caption(), "German (no dead keys)");
        assert!(!osd.has_level());
        let long = "Ü".repeat(40);
        let cut = Osd::input_source(&long).caption();
        assert!(cut.len() <= 48 && long.starts_with(&cut) && !cut.is_empty());
    }

    #[test]
    fn a_pad_mode_readout_counts_modes_from_one_without_a_bar() {
        let osd = Osd::pad_mode(1, 4);
        let caption = osd.caption();
        assert!(caption.contains('2') && caption.contains('4'), "{caption}");
        assert!(!osd.has_level());
    }

    #[test]
    fn osd_slab_geometry_is_centred_bounded_and_scaled_once() {
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

        slab_stack_is_centred_with_token_gaps_with_and_without_a_level_bar();
        slab_geometry_is_low_centered_and_width_bounded();
        caption_type_is_scaled_once_at_large_text();
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
                ("keyboard-backlight", Osd::keyboard_backlight(0.5)),
                ("no-backlight", Osd::no_backlight()),
                ("airplane", Osd::airplane_mode(true)),
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

    #[test]
    fn the_magnifier_readout_shows_the_zoom_on_a_bar_from_off_to_strongest() {
        use settings_store::magnifier::{MAX_ZOOM, OFF};
        let off = Osd::magnifier(OFF);
        assert_eq!(off.caption(), "Magnifier off");
        assert_eq!(off.level, 0.0);
        assert_eq!(Osd::magnifier(3).caption(), "Magnifier 3×");
        assert!(Osd::magnifier(3).has_level());
        assert_eq!(Osd::magnifier(MAX_ZOOM).level, 1.0);
        assert_eq!(
            Osd::magnifier(MAX_ZOOM + 5).kind,
            OsdKind::Magnifier { zoom: MAX_ZOOM }
        );
    }
}
