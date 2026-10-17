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
use ui::TextRole;

/// Offscreen wgpu + vello PNG writer shared by the system-surface review
/// fixtures (OSD, power menu, lock screen, notifications) — surfaces the
/// ux-bench cannot open on macOS, photographed straight from their paint
/// paths instead. The same harness `overlay_host` uses for its fixtures.
#[cfg(test)]
pub(crate) use ui::offscreen as review_offscreen;

use ui::text_render::{TextKind, TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
#[cfg(test)]
use vello::kurbo::Point;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

/// How long the readout stays up after the last press, fade included. Long
/// enough to read after a single tap, short enough that a burst of presses
/// does not leave it hanging around afterwards.
pub const OSD_HOLD: Duration = Duration::from_millis(1_500);

/// The fade-in, from the moment the readout first appears. Equal to the
/// motion token `durations.fast`; the lifecycle clock runs without a theme
/// in hand, so a test pins the two together.
pub const OSD_FADE_IN: Duration = Duration::from_millis(130);

/// The fade-out tail, inside [`OSD_HOLD`]'s trailing edge. Equal to the
/// motion token `durations.base`.
pub const OSD_FADE: Duration = Duration::from_millis(210);

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
    /// The ask-agent chord with no agent in the workspace live to take a
    /// task. Saying so beats a chord that silently does nothing.
    NoAgent,
    /// The ask-agent chord started the workspace's agent; the composer
    /// opens once it takes tasks.
    StartingAgent,
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
    /// stacking a second readout.
    pub raised_at: Instant,
    /// When the readout first appeared. A press that lands while it is up
    /// updates it in place and keeps this, so the fade-in is not replayed on
    /// every step of a volume burst.
    pub shown_since: Instant,
}

impl Osd {
    fn at(kind: OsdKind, level: f32) -> Self {
        let now = Instant::now();
        Self {
            kind,
            level,
            raised_at: now,
            shown_since: now,
        }
    }

    pub fn volume(level: f32, muted: bool) -> Self {
        Self::at(OsdKind::Volume { muted }, level.clamp(0.0, 1.0))
    }

    pub fn brightness(level: f32) -> Self {
        Self::at(OsdKind::Brightness, level.clamp(0.0, 1.0))
    }

    pub fn mic_mute(muted: bool) -> Self {
        Self::at(OsdKind::MicMute { muted }, 0.0)
    }

    pub fn pointer_held(held: bool) -> Self {
        Self::at(OsdKind::PointerHeld { held }, 0.0)
    }

    pub fn keys_captured(captured: bool) -> Self {
        Self::at(OsdKind::KeysCaptured { captured }, 0.0)
    }

    pub fn transport(t: Transport) -> Self {
        Self::at(OsdKind::Transport(t), 0.0)
    }

    pub fn keyboard_backlight(level: f32) -> Self {
        Self::at(OsdKind::KeyboardBacklight, level.clamp(0.0, 1.0))
    }

    pub fn no_backlight() -> Self {
        Self::at(OsdKind::NoBacklight, 0.0)
    }

    pub fn airplane_mode(on: bool) -> Self {
        Self::at(OsdKind::AirplaneMode { on }, 0.0)
    }

    pub fn touchpad(enabled: bool) -> Self {
        Self::at(OsdKind::Touchpad { enabled }, 0.0)
    }

    pub fn mouse_keys(enabled: bool) -> Self {
        Self::at(OsdKind::MouseKeys { enabled }, 0.0)
    }

    /// The magnifier at `zoom`; the bar fills from off to the strongest zoom.
    pub fn magnifier(zoom: u8) -> Self {
        use settings_store::magnifier::{MAX_ZOOM, OFF};
        let zoom = zoom.clamp(OFF, MAX_ZOOM);
        Self::at(
            OsdKind::Magnifier { zoom },
            f32::from(zoom.saturating_sub(OFF)) / f32::from(MAX_ZOOM.saturating_sub(OFF)),
        )
    }

    /// A tablet pad now in `mode` (from zero) of `modes`.
    pub fn pad_mode(mode: u32, modes: u32) -> Self {
        Self::at(OsdKind::PadMode { mode, modes }, 0.0)
    }

    pub fn no_agent() -> Self {
        Self::at(OsdKind::NoAgent, 0.0)
    }

    pub fn starting_agent() -> Self {
        Self::at(OsdKind::StartingAgent, 0.0)
    }

    pub fn input_source(name: &str) -> Self {
        Self::at(OsdKind::InputSource(OsdLabel::new(name)), 0.0)
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
            OsdKind::NoAgent => t("osd-no-agent"),
            OsdKind::StartingAgent => t("osd-starting-agent"),
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

    /// Opacity at `now`: easing in over [`OSD_FADE_IN`], opaque, then easing
    /// out over [`OSD_FADE`], then `0.0`.
    pub fn alpha_at(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.raised_at);
        let Some(remaining) = OSD_HOLD.checked_sub(elapsed) else {
            return 0.0;
        };
        let shown = now.saturating_duration_since(self.shown_since);
        let rise = if shown >= OSD_FADE_IN {
            1.0
        } else {
            // Ease-out: the readout arrives quickly and settles.
            let t = shown.as_secs_f32() / OSD_FADE_IN.as_secs_f32();
            1.0 - (1.0 - t) * (1.0 - t)
        };
        if remaining >= OSD_FADE {
            return rise;
        }
        let t = remaining.as_secs_f32() / OSD_FADE.as_secs_f32();
        // Ease-out so the last frames are the ones that thin out, rather
        // than a linear ramp that reads as a flicker at the end.
        rise * t * t
    }

    /// [`Self::alpha_at`] as `theme` shows it: under reduced motion the
    /// readout cuts in and out instead of fading and sliding.
    pub fn alpha_in(&self, theme: &Theme, now: Instant) -> f32 {
        if theme.reduce_motion {
            return if self.is_live(now) { 1.0 } else { 0.0 };
        }
        self.alpha_at(now)
    }

    /// Whether the readout should still be painted at `now`.
    pub fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.raised_at) < OSD_HOLD
    }

    /// Whether the readout is fading in or out and therefore needs a frame
    /// on every presentation tick. The opaque part of the hold is
    /// deliberately excluded: a static OSD should not keep the compositor
    /// repainting for more than a second.
    pub fn needs_animation_frame_at(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.raised_at);
        if elapsed >= OSD_HOLD {
            return false;
        }
        now.saturating_duration_since(self.shown_since) < OSD_FADE_IN
            || elapsed >= OSD_HOLD.saturating_sub(OSD_FADE)
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

    /// The short value shown beside the level bar (or, for a state with no
    /// level, the whole message): `62%`, `Muted`, `Off`, `3×`.
    pub fn value_text(&self) -> String {
        let percent = || {
            t_args(
                "osd-percent",
                &[("percent", level_to_percent(self.level).into())],
            )
        };
        match self.kind {
            OsdKind::Volume { muted: true } => t("osd-muted"),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-off"),
            OsdKind::Volume { muted: false } | OsdKind::Brightness | OsdKind::KeyboardBacklight => {
                percent()
            }
            OsdKind::Magnifier { zoom } if zoom <= settings_store::magnifier::OFF => t("osd-off"),
            OsdKind::Magnifier { zoom } => settings_store::magnifier::zoom_label(zoom).into(),
            _ => self.caption(),
        }
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
        // pill paints with those resolved sizes; the renderer must not scale
        // them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restored at the end.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let faded = alpha < 0.999;
        // The pill rises into place as it fades in and sinks as it fades
        // out; the slide follows the opacity, so it needs no clock of its
        // own and a reduced-motion theme (alpha pinned at 1) never moves.
        let slide = if theme.reduce_motion {
            0.0
        } else {
            f64::from(theme.tokens.spacing.s2) * f64::from(1.0 - alpha.clamp(0.0, 1.0))
        };
        let transform = transform.then_translate((0.0, slide).into());
        if faded {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &viewport.inflate(
                    f64::from(theme.tokens.spacing.s8),
                    f64::from(theme.tokens.spacing.s8),
                ),
            );
        }

        let text_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Medium)
            .with_kind(TextKind::Numeric);
        let pill = OsdPill::layout(viewport, theme, text_renderer, &self, &text_style);
        ui::chrome::shell::paint_panel(scene, transform, pill.rect, theme, 1.0);
        draw_glyph(scene, transform, pill.glyph, theme, self.kind);
        if let Some(bar) = pill.bar {
            draw_level_bar(scene, transform, bar, theme, self);
        }
        let muted = matches!(self.kind, OsdKind::Volume { muted: true });
        let colors = &theme.tokens.colors;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a pill width in logical pixels is far inside f32 range"
        )]
        let pill_text_width = pill.text.width() as f32;
        let text =
            text_renderer.elide_to_width_style(&pill.text_value, pill_text_width, &text_style);
        let (tw, th) = text_renderer.measure_style(&text, None, &text_style);
        // The value hugs the bar's end; a bare message starts at the glyph.
        let x = if pill.bar.is_some() {
            pill.text.x1 - f64::from(tw)
        } else {
            pill.text.x0
        };
        text_renderer.draw_style(
            scene,
            &text,
            if muted {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform.pre_translate((x, pill.text.center().y - f64::from(th) / 2.0).into()),
            None,
            &text_style,
        );

        if faded {
            scene.pop_layer();
        }
        text_renderer.set_text_scale(renderer_scale);
    }
}

/// The pill's geometry: glyph, optional level bar and value on one row,
/// bottom-centred in the viewport. Keeping layout separate from paint makes
/// the rhythm testable and lets every kind (level or state) share one shape.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OsdPill {
    pub rect: Rect,
    pub glyph: Rect,
    pub bar: Option<Rect>,
    /// Where the value text goes: right-aligned after the bar for a level,
    /// or the whole message after the glyph.
    pub text: Rect,
    pub text_value: String,
}

impl OsdPill {
    pub(crate) fn layout(
        viewport: Rect,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        osd: &Osd,
        style: &ui::text_render::TextStyle,
    ) -> Self {
        let sp = &theme.tokens.spacing;
        let icon = f64::from(theme.tokens.metrics.icon_lg);
        let (pad, gap) = (f64::from(sp.s4), f64::from(sp.s3));
        let text_value = osd.value_text();
        let (value_w, text_h) = text_renderer.measure_style(&text_value, None, style);
        let has_bar = osd.has_level();
        // A level's value slot is as wide as its widest reading, so the bar
        // does not shift as the number gains digits or the sink mutes.
        let mut slot = f64::from(value_w);
        if has_bar {
            let widest = [
                t_args("osd-percent", &[("percent", 100.into())]),
                t("osd-muted"),
            ];
            for reading in widest {
                let (w, _) = text_renderer.measure_style(&reading, None, style);
                slot = slot.max(f64::from(w));
            }
        }
        let bar_len = icon * 6.0;
        let wanted = pad * 2.0 + icon + gap + slot + if has_bar { gap + bar_len } else { 0.0 };
        let avail = (viewport.width() - 2.0 * f64::from(sp.s4)).max(0.0);
        let width = wanted.min(avail);
        let height = icon.max(f64::from(text_h)) + 2.0 * gap;
        let height = height.min(viewport.height());
        let x0 = viewport.center().x - width / 2.0;
        let y1 = (viewport.y1 - f64::from(sp.s16)).max(viewport.y0 + height);
        let rect = Rect::new(x0, y1 - height, x0 + width, y1);
        let cy = rect.center().y;
        let glyph = Rect::new(
            rect.x0 + pad,
            cy - icon / 2.0,
            rect.x0 + pad + icon,
            cy + icon / 2.0,
        );
        let text_x0 = if has_bar {
            rect.x1 - pad - slot
        } else {
            glyph.x1 + gap
        };
        let text = Rect::new(
            text_x0,
            cy - f64::from(text_h) / 2.0,
            rect.x1 - pad,
            cy + f64::from(text_h) / 2.0,
        );
        let bar = has_bar.then(|| {
            let bar_h = f64::from(sp.s1) * 1.5;
            Rect::new(
                glyph.x1 + gap,
                cy - bar_h / 2.0,
                text_x0 - gap,
                cy + bar_h / 2.0,
            )
        });
        Self {
            rect,
            glyph,
            bar,
            text,
            text_value,
        }
    }
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
        OsdKind::NoAgent => (ui::icons::Icon::Bot, colors.text_dim),
        OsdKind::StartingAgent => (ui::icons::Icon::Bot, colors.text_primary),
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
    // The empty track is a hairline-polarity wash over the pill's own
    // ground, so it reads on every palette — a lift of the desktop base
    // can land below the Panel ground and vanish.
    scene.fill(
        Fill::NonZero,
        transform,
        track_color(theme),
        None,
        &RoundedRect::from_rect(bar, bar_h / 2.0),
    );
    // Muting keeps the track but empties its fill, so the pill does not
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
            colors.text_primary,
            None,
            &RoundedRect::from_rect(fill_rect, bar_h / 2.0),
        );
    }
}

/// The empty part of the level bar.
pub(crate) fn track_color(theme: &Theme) -> vello::peniko::Color {
    ui::tokens::colors::Colors::hairline_on(ui::chrome::shell::panel_ground(theme), 80)
}

/// Same key restarts the timer, different key replaces outright — either
/// way "never stack" lives in one place, not each caller.
pub fn raise(current: &mut Option<Osd>, mut next: Osd) {
    // A readout that is still up is updated in place: the timer restarts but
    // the fade-in is not replayed.
    if let Some(live) = current.filter(|o| o.is_live(next.raised_at)) {
        next.shown_since = live.shown_since;
    }
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
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn osd_slab_fades_over_its_tail_and_is_replaced_not_stacked() {
        fn a_fresh_slab_is_opaque_and_an_old_one_is_gone() {
            let o = Osd::volume(0.5, false);
            let t0 = o.raised_at;
            assert_eq!(o.alpha_at(t0), 0.0, "it fades in rather than popping");
            assert_eq!(o.alpha_at(t0 + OSD_FADE_IN), 1.0);
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

            assert!(o.needs_animation_frame_at(t0), "the fade-in animates");
            let opaque = t0 + OSD_FADE_IN;
            assert!(!o.needs_animation_frame_at(opaque));
            assert_eq!(o.next_wake_deadline_at(opaque), Some(fade_start));
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

        fn an_update_while_up_keeps_the_fade_in_it_already_finished() {
            let mut cur = Some(Osd::volume(0.3, false));
            let first = cur.unwrap();
            let later = first.raised_at + Duration::from_millis(600);
            let mut next = Osd::volume(0.4, false);
            next.raised_at = later;
            raise(&mut cur, next);
            let shown = cur.unwrap();
            assert_eq!(shown.shown_since, first.shown_since);
            assert_eq!(shown.alpha_at(later), 1.0, "a burst does not blink");
        }

        fn motion_tokens_back_the_lifecycle_constants() {
            let d = ui::tokens::motion::Durations::default();
            assert_eq!(OSD_FADE_IN, d.fast());
            assert_eq!(OSD_FADE, d.base());
            let theme = Theme::dark().with_reduce_motion(true);
            let o = Osd::volume(0.5, false);
            assert_eq!(o.alpha_in(&theme, o.raised_at), 1.0, "cuts in");
            assert_eq!(o.alpha_in(&theme, o.raised_at + OSD_HOLD), 0.0, "and out");
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
        an_update_while_up_keeps_the_fade_in_it_already_finished();
        motion_tokens_back_the_lifecycle_constants();
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

    fn pill(osd: &Osd, theme: &Theme, viewport: Rect) -> OsdPill {
        let mut text = TextRenderer::new();
        text.set_text_scale(1.0);
        let style = theme.text(TextRole::Label);
        OsdPill::layout(viewport, theme, &mut text, osd, &style)
    }

    #[test]
    fn osd_pill_geometry_is_bottom_centred_bounded_and_scaled_once() {
        fn every_kind_is_one_row_in_the_bottom_third_on_theme_tokens() {
            let theme = Theme::dark();
            let viewport = Rect::new(20.0, 40.0, 1220.0, 840.0);
            let icon = f64::from(theme.tokens.metrics.icon_lg);
            let s3 = f64::from(theme.tokens.spacing.s3);
            for osd in [
                Osd::volume(0.5, false),
                Osd::volume(0.5, true),
                Osd::brightness(0.4),
                Osd::keyboard_backlight(0.0),
                Osd::mic_mute(true),
                Osd::transport(Transport::Next),
                Osd::no_backlight(),
            ] {
                let p = pill(&osd, &theme, viewport);
                assert!(
                    (p.rect.center().x - viewport.center().x).abs() < 1e-6,
                    "{:?}",
                    osd.kind
                );
                assert!(p.rect.center().y > viewport.y0 + viewport.height() * 2.0 / 3.0);
                assert_eq!(viewport.y1 - p.rect.y1, f64::from(theme.tokens.spacing.s16));
                assert!(p.rect.height() >= icon + 2.0 * s3 - 1e-6);
                assert!(p.rect.x0 >= viewport.x0 && p.rect.x1 <= viewport.x1);
                assert!(p.glyph.x1 <= p.text.x0 && p.glyph.width() == icon);
                if let Some(bar) = p.bar {
                    assert!(bar.x0 >= p.glyph.x1 && bar.x1 <= p.text.x0);
                    assert!(
                        bar.width() > icon * 4.0,
                        "a level bar is long enough to read"
                    );
                }
            }
        }

        fn the_bar_does_not_move_when_the_reading_changes() {
            let theme = Theme::dark();
            let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let a = pill(&Osd::volume(0.05, false), &theme, viewport);
            let b = pill(&Osd::volume(1.0, false), &theme, viewport);
            let c = pill(&Osd::volume(1.0, true), &theme, viewport);
            assert_eq!(a.rect, b.rect);
            assert_eq!(a.bar, b.bar);
            assert_eq!(a.rect, c.rect, "muting keeps the pill's shape");
        }

        fn a_narrow_viewport_bounds_the_pill() {
            let theme = Theme::dark();
            let tiny = Rect::new(5.0, 7.0, 205.0, 87.0);
            let p = pill(&Osd::transport(Transport::Previous), &theme, tiny);
            assert!(p.rect.x0 >= tiny.x0 && p.rect.x1 <= tiny.x1 && p.rect.y0 >= tiny.y0);
        }

        /// HIG §7a: the value is scaled exactly once. The pass paints the
        /// theme-resolved label with the renderer reset to 1×, so at 200 % the
        /// line is twice the 1× line — not four times — and the renderer's own
        /// scale is handed back to the widgets that follow.
        fn type_is_scaled_once_at_large_text() {
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
            let mut at_pass = TextRenderer::new();
            let (_, h1) = at_pass.measure_style("50%", None, &base.text(TextRole::Label));
            let (_, h2) = at_pass.measure_style("50%", None, &large.text(TextRole::Label));
            assert!(
                (h2 - 2.0 * h1).abs() < 1.0,
                "200 % line {h2} should be twice {h1}"
            );
            let (small, big) = (
                pill(&Osd::brightness(0.5), &base, viewport),
                pill(&Osd::brightness(0.5), &large, viewport),
            );
            assert!(
                big.rect.height() > small.rect.height(),
                "the pill follows the text scale"
            );
        }

        fn the_value_reads_short_beside_the_bar() {
            assert_eq!(Osd::volume(0.375, false).value_text(), "38%");
            assert_eq!(Osd::volume(0.375, true).value_text(), "Muted");
            assert_eq!(Osd::keyboard_backlight(0.0).value_text(), "Off");
            assert_eq!(Osd::brightness(1.0).value_text(), "100%");
            assert_eq!(Osd::transport(Transport::Stop).value_text(), "Stop");
        }

        every_kind_is_one_row_in_the_bottom_third_on_theme_tokens();
        the_bar_does_not_move_when_the_reading_changes();
        a_narrow_viewport_bounds_the_pill();
        type_is_scaled_once_at_large_text();
        the_value_reads_short_beside_the_bar();
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
                ("volume-muted", Osd::volume(0.62, true)),
                ("brightness", Osd::brightness(0.4)),
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
                let slab = pill(&osd, &theme, viewport).rect;
                let probe = Point::new(slab.center().x, slab.y0 + 3.0);
                assert_pixel_is(
                    &rgba,
                    w,
                    probe,
                    ui::chrome::shell::panel_ground(&theme),
                    &format!("{name}-{kind}: pill interior is the Panel ground"),
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
