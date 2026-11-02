//! The transient readout for the hardware keys: volume, brightness, the
//! keyboard's backlight, playback and the machine toggles.
//!
//! A key that changes something invisible (the volume) or unreadable (the
//! brightness, *because* it is what changed) needs an answer on screen: a
//! slab that appears, shows the new level, and gets out of the way. It never
//! takes focus or the pointer and never outlives the moment it was about.
//! It is deliberately not a notification, which persists and stacks:
//! pressing volume up eight times shows one slab counting up, not eight
//! banners.
//!
//! Whoever handled the key shows it through its published action
//! (`desktop:osd/show`), as any caller would.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use taffy::{NodeId, Style, TaffyTree};
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode, OperationKind,
    ParamType, ResourceSpec, ResultSpec,
};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextWeight};
use ui::time::{Duration, Instant};
use ui::tokens::TextRole;
use ui::widget::{TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use crate::i18n::{t, t_args};

/// The operation type the readout's action carries.
pub const OPERATION: &str = "tilo.desktop.osd";
/// The readout's resource.
pub const OSD_RESOURCE: &str = "desktop:osd";

/// How long the readout stays up after the last change, fade included:
/// long enough to read after one tap, short enough that a burst of presses
/// does not leave it hanging around.
pub const OSD_HOLD: Duration = Duration::from_millis(1_500);

/// The pill's height: its glyph's square and the gap above and below it.
/// Unscaled `BASE` values because a host sizes the surface from this before
/// any theme is in hand.
pub const OSD_HEIGHT: f32 = ui::tokens::Metrics::BASE.icon_lg + 2.0 * ui::tokens::Spacing::BASE.s3;

/// Room above and below the pill's body for the shadow it floats on: a
/// surface sized to the body alone would cut the shadow off in a hard edge.
pub const OSD_MARGIN: f32 = ui::tokens::Spacing::BASE.s8;

/// How far the pill's bottom edge stands above the output's bottom edge: the
/// same inset the readouts have always rested at, where a glance finds them
/// and no bar or dock sits.
pub const OSD_INSET: f32 = ui::tokens::Spacing::BASE.s16;

/// The height of the surface a host shows the readout on: the pill and its
/// shadow's room, in whole logical pixels.
#[must_use]
pub fn surface_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's height in logical pixels is a few dozen"
    )]
    let height = (OSD_HEIGHT + 2.0 * OSD_MARGIN).ceil() as i32;
    height
}

/// Where the readout's surface goes on the output `(x, y, w, h)`: a band
/// across the output, its pill bottom-centred in it, [`OSD_INSET`] above the
/// output's bottom edge. The pill is as wide as what it reads, so the band is
/// as wide as the output and the pill centres itself in it.
#[must_use]
pub fn place(output: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "insets of a few dozen logical pixels"
    )]
    let (inset, margin) = (OSD_INSET.round() as i32, OSD_MARGIN.round() as i32);
    let (x, y, w, h) = output;
    let height = surface_height();
    let bottom = y
        .saturating_add(h)
        .saturating_sub(inset)
        .saturating_add(margin);
    (x, bottom.saturating_sub(height), w.max(1), height)
}

/// What the readout is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsdKind {
    Volume,
    Brightness,
    KeyboardBacklight,
    Microphone,
    AirplaneMode,
    Touchpad,
    Play,
    Pause,
    NextTrack,
    PreviousTrack,
    Stop,
    /// A brightness key on a machine with no display that can be dimmed.
    NoBacklight,
}

impl OsdKind {
    /// Every kind, as its published name spells it.
    pub const ALL: [Self; 12] = [
        Self::Volume,
        Self::Brightness,
        Self::KeyboardBacklight,
        Self::Microphone,
        Self::AirplaneMode,
        Self::Touchpad,
        Self::Play,
        Self::Pause,
        Self::NextTrack,
        Self::PreviousTrack,
        Self::Stop,
        Self::NoBacklight,
    ];

    /// Whether a level bar shows beside the caption.
    #[must_use]
    pub const fn has_level(self) -> bool {
        matches!(
            self,
            Self::Volume | Self::Brightness | Self::KeyboardBacklight
        )
    }
}

/// One readout: what it is about, its level (`0.0 ..= 1.0`) and whether
/// the thing is on (sound not muted, the radio or touchpad on).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Osd {
    pub kind: OsdKind,
    #[serde(default)]
    pub level: f32,
    #[serde(default = "on")]
    pub on: bool,
}

const fn on() -> bool {
    true
}

/// `level` as a whole percentage.
fn percent(level: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the level is clamped to 0..=1, so the percentage is 0..=100"
    )]
    let percent = (level.clamp(0.0, 1.0) * 100.0).round() as u32;
    percent
}

impl Osd {
    /// The one line the slab reads.
    #[must_use]
    pub fn caption(&self) -> String {
        let level = || [("percent", percent(self.level).into())];
        match self.kind {
            OsdKind::Volume if !self.on => t("osd-muted"),
            OsdKind::Volume => t_args("osd-volume", &level()),
            OsdKind::Brightness => t_args("osd-brightness", &level()),
            OsdKind::NoBacklight => t("osd-no-backlight"),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-keyboard-backlight-off"),
            OsdKind::KeyboardBacklight => t_args("osd-keyboard-backlight", &level()),
            OsdKind::Microphone if self.on => t("osd-microphone-on"),
            OsdKind::Microphone => t("osd-microphone-muted"),
            OsdKind::AirplaneMode if self.on => t("osd-airplane-mode-on"),
            OsdKind::AirplaneMode => t("osd-airplane-mode-off"),
            OsdKind::Touchpad if self.on => t("osd-touchpad-on"),
            OsdKind::Touchpad => t("osd-touchpad-off"),
            OsdKind::Play => t("osd-play"),
            OsdKind::Pause => t("osd-pause"),
            OsdKind::NextTrack => t("osd-next-track"),
            OsdKind::PreviousTrack => t("osd-previous-track"),
            OsdKind::Stop => t("osd-stop"),
        }
    }

    /// The short value the pill reads beside its glyph: `65%`, `Muted`,
    /// `Off`, or the whole message where there is no level to read.
    #[must_use]
    pub fn value_text(&self) -> String {
        let percent = || t_args("osd-percent", &[("percent", percent(self.level).into())]);
        match self.kind {
            OsdKind::Volume if !self.on => t("osd-muted"),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-off"),
            OsdKind::Volume | OsdKind::Brightness | OsdKind::KeyboardBacklight => percent(),
            _ => self.caption(),
        }
    }

    /// The glyph for what the readout is about, and whether it reads as on:
    /// a state that is off (muted, dark, radio down) dims its glyph and, for
    /// the microphone, warns.
    #[must_use]
    pub fn glyph(&self) -> (ui::icons::Icon, GlyphTone) {
        use ui::icons::Icon;
        match self.kind {
            OsdKind::Volume if self.on => (Icon::Volume2, GlyphTone::On),
            OsdKind::Volume => (Icon::VolumeX, GlyphTone::On),
            OsdKind::Brightness => (Icon::Sun, GlyphTone::On),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => (Icon::Keyboard, GlyphTone::Off),
            OsdKind::KeyboardBacklight => (Icon::Keyboard, GlyphTone::On),
            OsdKind::Microphone if self.on => (Icon::Mic, GlyphTone::On),
            OsdKind::Microphone => (Icon::MicOff, GlyphTone::Warn),
            OsdKind::AirplaneMode if self.on => (Icon::WifiOff, GlyphTone::On),
            OsdKind::AirplaneMode => (Icon::Wifi, GlyphTone::On),
            OsdKind::Touchpad if self.on => (Icon::Mouse, GlyphTone::On),
            OsdKind::Touchpad => (Icon::Mouse, GlyphTone::Off),
            OsdKind::Play => (Icon::Play, GlyphTone::On),
            OsdKind::Pause => (Icon::Pause, GlyphTone::On),
            OsdKind::NextTrack => (Icon::SkipForward, GlyphTone::On),
            OsdKind::PreviousTrack => (Icon::SkipBack, GlyphTone::On),
            OsdKind::Stop => (Icon::Square, GlyphTone::On),
            OsdKind::NoBacklight => (Icon::Monitor, GlyphTone::Off),
        }
    }

    /// How much of the level track is filled: a muted volume keeps its track
    /// and empties its fill, so the pill does not change shape while still
    /// agreeing with the crossed-out volume glyph.
    #[must_use]
    pub fn filled(&self) -> f64 {
        if self.kind == OsdKind::Volume && !self.on {
            0.0
        } else if self.kind.has_level() {
            f64::from(self.level.clamp(0.0, 1.0))
        } else {
            0.0
        }
    }
}

/// How a readout's glyph is inked: lit, dimmed (the thing is off) or
/// warning (the microphone is muted and the person may be talking to it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphTone {
    On,
    Off,
    Warn,
}

/// The pill's geometry: its glyph, optional level track and value on one
/// row, bottom-centred in the surface's band. Kept apart from paint so the
/// rhythm is testable and every kind, level or state, shares one shape.
#[derive(Clone, Debug, PartialEq)]
struct Pill {
    rect: Rect,
    glyph: Rect,
    /// The level track between the glyph and the value, for a kind with a
    /// level.
    track: Option<Rect>,
    /// Where the value goes: right-aligned in its slot after the track for a
    /// level, or straight after the glyph for a bare message.
    text: Rect,
    value: String,
}

impl Pill {
    /// Lay the pill out in the surface's `band`, whose pill is
    /// [`OSD_HEIGHT`] tall and as wide as its widest reading.
    fn layout(
        band: Rect,
        theme: &Theme,
        text: &mut TextRenderer,
        osd: Osd,
        style: &ui::text_render::TextStyle,
    ) -> Self {
        let sp = &theme.tokens.spacing;
        let icon = f64::from(theme.tokens.metrics.icon_lg);
        let (pad, gap) = (f64::from(sp.s4), f64::from(sp.s3));
        let value = osd.value_text();
        let (value_w, text_h) = text.measure_style(&value, None, style);
        let has_track = osd.kind.has_level();
        // A level's value slot is as wide as its widest reading, so the
        // track does not shift as the number gains digits or the sink mutes.
        let mut slot = f64::from(value_w);
        if has_track {
            let full = t_args("osd-percent", &[("percent", 100.into())]);
            for reading in [full, t("osd-muted")] {
                let (w, _) = text.measure_style(&reading, None, style);
                slot = slot.max(f64::from(w));
            }
        }
        let track_len = icon * 6.0;
        let wanted = pad * 2.0 + icon + gap + slot + if has_track { gap + track_len } else { 0.0 };
        let width = wanted.min(band.width()).max(0.0);
        // As tall as its glyph or its line, and the gap above and below,
        // never taller than the band has room for.
        let room = (band.height() - 2.0 * f64::from(OSD_MARGIN)).max(0.0);
        let height = (icon.max(f64::from(text_h)) + 2.0 * gap).min(room);
        // Bottom-centred: the pill grows upward from the line it rests on.
        let x0 = band.center().x - width / 2.0;
        let y1 = band.y1 - f64::from(OSD_MARGIN);
        let rect = Rect::new(x0, y1 - height, x0 + width, y1);
        let cy = rect.center().y;
        let glyph = Rect::new(
            rect.x0 + pad,
            cy - icon / 2.0,
            rect.x0 + pad + icon,
            cy + icon / 2.0,
        );
        let text_x0 = if has_track {
            rect.x1 - pad - slot
        } else {
            glyph.x1 + gap
        };
        let text_rect = Rect::new(
            text_x0,
            cy - f64::from(text_h) / 2.0,
            rect.x1 - pad,
            cy + f64::from(text_h) / 2.0,
        );
        let track = has_track.then(|| {
            let track_h = f64::from(sp.s1) * 1.5;
            Rect::new(
                glyph.x1 + gap,
                cy - track_h / 2.0,
                text_x0 - gap,
                cy + track_h / 2.0,
            )
        });
        Self {
            rect,
            glyph,
            track,
            text: text_rect,
            value,
        }
    }
}

/// The empty part of the level track: a hairline wash over the pill's own
/// ground, so it reads on every palette rather than vanishing under a lift
/// of the desktop's base.
fn track_color(theme: &Theme) -> vello::peniko::Color {
    ui::tokens::colors::Colors::hairline_on(ui::chrome::shell::panel_ground(theme), 80)
}

/// Paint the readout's pill in `band`, returning its resting rect.
fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    raised: Osd,
) -> Rect {
    let style = theme
        .text(TextRole::Label)
        .with_weight(TextWeight::Medium)
        .with_kind(TextKind::Numeric);
    let pill = Pill::layout(band, theme, text, raised, &style);
    let painted = ui::chrome::shell::paint_panel(scene, transform, pill.rect, theme, 1.0);
    let theme = &painted.theme;
    let colors = &theme.tokens.colors;
    let (icon, tone) = raised.glyph();
    let tint = match tone {
        GlyphTone::On => colors.text_primary,
        GlyphTone::Off => colors.text_dim,
        GlyphTone::Warn => colors.semantic.warning,
    };
    ui::icons::draw_icon(
        scene,
        transform,
        pill.glyph,
        icon,
        tint,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX * 1.4,
    );
    if let Some(track) = pill.track {
        let track_h = track.height();
        scene.fill(
            Fill::NonZero,
            transform,
            track_color(theme),
            None,
            &vello::kurbo::RoundedRect::from_rect(track, track_h / 2.0),
        );
        let filled = raised.filled();
        if filled > 0.0 {
            let w = (track.width() * filled).max(track_h);
            let fill = Rect::new(track.x0, track.y0, track.x0 + w, track.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary,
                None,
                &vello::kurbo::RoundedRect::from_rect(fill, track_h / 2.0),
            );
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's value room in logical pixels is far inside f32 range"
    )]
    let room = pill.text.width() as f32;
    let shown = text.elide_to_width_style(&pill.value, room, &style);
    let (shown_w, shown_h) = text.measure_style(&shown, None, &style);
    let muted = raised.kind == OsdKind::Volume && !raised.on;
    // The value hugs the track's end; a bare message starts at the glyph.
    let x = if pill.track.is_some() {
        pill.text.x1 - f64::from(shown_w)
    } else {
        pill.text.x0
    };
    text.draw_style(
        scene,
        &shown,
        if muted {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((x, pill.text.center().y - f64::from(shown_h) / 2.0).into()),
        None,
        &style,
    );
    pill.rect
}

#[must_use]
#[expect(clippy::expect_used, reason = "a readout always serializes to JSON")]
pub fn osd_resource() -> ResourceSpec {
    let mut spec = ActionSpec::typed_operation(
        "show",
        t("desktop-action-show-osd"),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        vec![
            ActionParam::required_owned(
                "kind",
                ParamType::Text,
                t("desktop-action-param-osd-kind"),
            ),
            ActionParam::optional_owned(
                "level",
                ParamType::Number,
                t("desktop-action-param-osd-level"),
            ),
            ActionParam::optional_owned("on", ParamType::Boolean, t("desktop-action-param-osd-on")),
        ],
        |args| {
            let kind = args
                .text("kind")
                .and_then(|kind| {
                    serde_json::from_value::<OsdKind>(serde_json::Value::String(kind.into())).ok()
                })
                .unwrap_or(OsdKind::Volume);
            #[allow(clippy::cast_possible_truncation, reason = "a level between 0 and 1")]
            let level = args.number("level").unwrap_or(0.0) as f32;
            let osd = Osd {
                kind,
                level: level.clamp(0.0, 1.0),
                on: args.boolean("on").unwrap_or(true),
            };
            EncodedAction::encode_operation(OPERATION, &osd).expect("a readout always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    ResourceSpec::new(OSD_RESOURCE, "desktop", t("desktop-actions-osd")).with_action(spec)
}

/// The readout up now, and since when.
#[derive(Clone, Copy, Debug)]
struct Raised {
    osd: Osd,
    /// When this run of readouts appeared: a burst of presses keeps it.
    shown_since: Instant,
    /// When it last changed: it stays [`OSD_HOLD`] after.
    changed_at: Instant,
}

/// The slab, as a widget filling the rectangle its host gives it.
pub struct OsdSlab {
    raised: Option<Raised>,
    rect: Rect,
}

impl Default for OsdSlab {
    fn default() -> Self {
        Self::new()
    }
}

impl OsdSlab {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raised: None,
            rect: Rect::ZERO,
        }
    }

    /// Show `osd` now: a readout already up changes in place and stays up
    /// longer.
    pub fn show(&mut self, osd: Osd) {
        let now = ui::time::now();
        let shown_since = self
            .raised
            .filter(|raised| live(raised, now))
            .map_or(now, |raised| raised.shown_since);
        self.raised = Some(Raised {
            osd,
            shown_since,
            changed_at: now,
        });
    }

    /// The readout up now, if any.
    #[must_use]
    pub fn current(&self) -> Option<Osd> {
        let now = ui::time::now();
        self.raised
            .filter(|raised| live(raised, now))
            .map(|raised| raised.osd)
    }
}

/// When `raised` is gone.
fn expires(raised: &Raised) -> Instant {
    raised
        .changed_at
        .checked_add(OSD_HOLD)
        .unwrap_or(raised.changed_at)
}

/// Whether `raised` is still up at `now`.
fn live(raised: &Raised, now: Instant) -> bool {
    now < expires(raised)
}

/// How opaque `raised` is at `now`: easing in, held, easing out.
fn alpha(raised: &Raised, now: Instant) -> f32 {
    ui::motion::Transient::opacity(
        ui::motion::Transient::READOUT,
        (raised.shown_since, expires(raised)),
        now,
        ui::tokens::EffectiveMotion::default(),
    )
}

impl ui::resources::PublishesResources for OsdSlab {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![osd_resource()]
    }
}

impl ui::host_operation::ResourceOperations for OsdSlab {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        _doc: &mut ui::widget::Edits<'_>,
        _path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped().filter(|request| request.type_id == OPERATION) else {
            return Ok(None);
        };
        let osd: Osd =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        self.show(osd);
        Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl Widget for OsdSlab {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let now = ui::time::now();
        let Some(raised) = self.raised else {
            return;
        };
        if !live(&raised, now) {
            self.raised = None;
            return;
        }
        let alpha = alpha(&raised, now);
        if alpha <= 0.0 {
            return;
        }
        // The pill rises into place as it fades in and sinks as it fades out;
        // the slide follows the opacity, so it needs no clock of its own and a
        // reduced-motion theme (alpha pinned at 1) never moves.
        let slide = if theme.reduce_motion {
            0.0
        } else {
            f64::from(theme.tokens.spacing.s2) * f64::from(1.0 - alpha.clamp(0.0, 1.0))
        };
        let transform = transform.then_translate((0.0, slide).into());
        if alpha < 0.999 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &self.rect,
            );
        }
        draw_pill(scene, transform, text, theme, self.rect, raised.osd);
        if alpha < 0.999 {
            scene.pop_layer();
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let osd = self.current()?;
        Some(
            AccessNode::new(Role::Label)
                .with_name(osd.caption())
                .with_live(Politeness::Polite),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.raised
            .is_some_and(|raised| live(&raised, ui::time::now()))
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "OsdSlab"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_read_the_level_and_the_state() {
        ui::i18n::I18nManager::init("en-US");
        let volume = Osd {
            kind: OsdKind::Volume,
            level: 0.554,
            on: true,
        };
        assert_eq!(volume.caption(), "Volume 55%");
        assert_eq!(volume.value_text(), "55%");
        assert_eq!(
            Osd {
                on: false,
                ..volume
            }
            .caption(),
            "Muted"
        );
        assert_eq!(
            Osd {
                on: false,
                ..volume
            }
            .value_text(),
            "Muted"
        );
        assert_eq!(
            Osd {
                kind: OsdKind::Touchpad,
                level: 0.0,
                on: false,
            }
            .caption(),
            "Touchpad off"
        );
        assert_eq!(
            Osd {
                kind: OsdKind::KeyboardBacklight,
                level: 0.0,
                on: true,
            }
            .value_text(),
            "Off"
        );
        assert_eq!(
            Osd {
                kind: OsdKind::KeyboardBacklight,
                level: 0.4,
                on: true,
            }
            .value_text(),
            "40%"
        );
    }

    /// A 1280 by 800 output's readout band.
    fn band() -> Rect {
        let (x, y, w, h) = place((0, 0, 1280, 800));
        Rect::new(
            f64::from(x),
            f64::from(y),
            f64::from(x.saturating_add(w)),
            f64::from(y.saturating_add(h)),
        )
    }

    /// The pill `osd` lays out in the band of a 1280 by 800 output.
    fn pill(osd: Osd) -> Pill {
        let theme = Theme::dark();
        let style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Medium)
            .with_kind(TextKind::Numeric);
        Pill::layout(band(), &theme, &mut TextRenderer::new(), osd, &style)
    }

    fn volume(level: f32) -> Osd {
        Osd {
            kind: OsdKind::Volume,
            level,
            on: true,
        }
    }

    #[test]
    fn the_readout_rests_bottom_centred_where_the_old_shell_put_it() {
        let pill = pill(volume(0.65));
        // The old shell's pill on this output: a body 260 wide by 48 high at
        // x510..769, y688..735, its shadow reaching a few pixels further and
        // its bottom edge 64 above the screen's.
        assert_eq!(pill.rect.y1, 736.0);
        assert_eq!(pill.rect.y0, 688.0);
        assert_eq!(pill.rect.center().x, 640.0);
        assert!(pill.rect.width() < 320.0, "{}", pill.rect.width());
        // The glyph starts the pill's left inset and is a square icon.
        assert_eq!(pill.glyph.x0, pill.rect.x0 + 16.0);
        assert_eq!(pill.glyph.width(), pill.glyph.height());
        // The value ends it, right-aligned an inset from the right edge.
        assert_eq!(pill.text.x1, pill.rect.x1 - 16.0);
        // The track runs between them, six icons long.
        let track = pill.track.expect("a level has a track");
        assert_eq!(track.width(), pill.glyph.width() * 6.0);
        assert!(track.x0 > pill.glyph.x1 && track.x1 <= pill.text.x0);
        assert!(track.height() < 8.0, "a hairline, not a bar");
        assert_eq!(pill.value, "65%");
    }

    #[test]
    fn a_level_pill_keeps_its_width_as_the_reading_changes() {
        let wide = pill(volume(0.65)).rect.width();
        for other in [
            volume(0.05),
            volume(1.0),
            Osd {
                on: false,
                ..volume(0.65)
            },
        ] {
            assert_eq!(pill(other).rect.width(), wide, "{other:?} resized the pill");
        }
        // And a bare state is as wide as its own message, not a level's slot.
        let touchpad = Osd {
            kind: OsdKind::Touchpad,
            level: 0.0,
            on: true,
        };
        assert!(pill(touchpad).track.is_none());
        assert_eq!(pill(touchpad).text.x0, pill(touchpad).glyph.x1 + 12.0);
    }

    #[test]
    fn the_track_empties_when_the_sound_is_muted() {
        assert!((volume(0.37).filled() - 0.37).abs() < 1e-6);
        assert_eq!(
            Osd {
                on: false,
                ..volume(0.37)
            }
            .filled(),
            0.0
        );
        assert_eq!(
            Osd {
                kind: OsdKind::Touchpad,
                level: 0.5,
                on: true,
            }
            .filled(),
            0.0,
            "a state with no level has no track to fill"
        );
        // Out-of-range levels are clamped, never drawn past the track.
        assert_eq!(volume(2.0).filled(), 1.0);
        assert_eq!(volume(-1.0).filled(), 0.0);
    }

    #[test]
    fn every_kind_names_its_own_glyph_and_dims_when_it_is_off() {
        use ui::icons::Icon;
        let glyph = |kind, level, on| Osd { kind, level, on }.glyph();
        assert_eq!(
            glyph(OsdKind::Volume, 0.5, true),
            (Icon::Volume2, GlyphTone::On)
        );
        assert_eq!(
            glyph(OsdKind::Volume, 0.5, false),
            (Icon::VolumeX, GlyphTone::On)
        );
        assert_eq!(
            glyph(OsdKind::Brightness, 0.5, true),
            (Icon::Sun, GlyphTone::On)
        );
        assert_eq!(
            glyph(OsdKind::KeyboardBacklight, 0.5, true),
            (Icon::Keyboard, GlyphTone::On)
        );
        assert_eq!(
            glyph(OsdKind::KeyboardBacklight, 0.0, true),
            (Icon::Keyboard, GlyphTone::Off)
        );
        assert_eq!(
            glyph(OsdKind::Microphone, 0.0, false),
            (Icon::MicOff, GlyphTone::Warn)
        );
        assert_eq!(
            glyph(OsdKind::Touchpad, 0.0, false),
            (Icon::Mouse, GlyphTone::Off)
        );
        assert_eq!(
            glyph(OsdKind::NextTrack, 0.0, true),
            (Icon::SkipForward, GlyphTone::On)
        );
        assert_eq!(
            glyph(OsdKind::NoBacklight, 0.0, true),
            (Icon::Monitor, GlyphTone::Off)
        );
        // Every kind the action accepts has a glyph of its own.
        for kind in OsdKind::ALL {
            let (_, tone) = Osd {
                kind,
                level: 0.0,
                on: true,
            }
            .glyph();
            assert!(matches!(
                tone,
                GlyphTone::On | GlyphTone::Off | GlyphTone::Warn
            ));
        }
    }

    #[test]
    fn a_readout_shows_by_its_action_changes_in_place_and_goes() {
        let _settled = ui::motion::testing::settled();
        let mut slab = OsdSlab::new();
        assert!(slab.current().is_none());
        let resource = osd_resource();
        let spec = &resource.actions[0];
        let mut arguments = serde_json::Map::new();
        arguments.insert("kind".into(), "brightness".into());
        arguments.insert("level".into(), 0.3.into());
        let encoded = tilo_actions::door::encode_json(
            std::slice::from_ref(&resource),
            &tilo_actions::ActionAddress::new(OSD_RESOURCE, spec.id.as_str()),
            &arguments,
        )
        .unwrap();
        let osd: Osd = serde_json::from_slice(&encoded.payload).unwrap();
        slab.show(osd);
        assert_eq!(
            slab.current().map(|osd| osd.kind),
            Some(OsdKind::Brightness)
        );
        slab.show(Osd { level: 0.4, ..osd });
        assert_eq!(slab.current().map(|osd| osd.level), Some(0.4));
    }
}
