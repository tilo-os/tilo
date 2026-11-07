//! The transient readout for the hardware keys: volume, brightness, the
//! keyboard's backlight, playback and the machine toggles.
//!
//! A key that changes something invisible (the volume) or unreadable (the
//! brightness, *because* it is what changed) needs an answer on screen: a
//! slab that appears, says **which device it changed**, shows the new level
//! as a bar, and gets out of the way. It never takes focus or the pointer and
//! never outlives the moment it was about. It is deliberately not a
//! notification, which persists and stacks: pressing volume up eight times
//! shows one slab counting up, not eight banners.
//!
//! Two shapes, one action: [`Osd::Level`] for something read as an amount (a
//! volume, a backlight) and [`Osd::Hint`] for a mode whose new state words are
//! enough (night light, Do Not Disturb, a chord taken). Whoever handled the
//! key shows it through its published action (`desktop:osd/show`), as any
//! caller would.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use taffy::{NodeId, Style, TaffyTree};
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode, OperationKind,
    ParamType, ResourceSpec, ResultSpec,
};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::motion::{Direction, MotionValue, Presence, Transient};
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextStyle, TextWeight};
use ui::time::{Duration, Instant};
use ui::tokens::{EffectiveMotion, Motion, MotionSource, TextRole};
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

/// The hint pill's height: its glyph's square and the gap above and below it.
/// Unscaled `BASE` values because a host sizes the surface from this before
/// any theme is in hand.
pub const OSD_ROW_HEIGHT: f32 =
    ui::tokens::Metrics::BASE.icon_lg + 2.0 * ui::tokens::Spacing::BASE.s3;

/// The level pill's height: the device's row, the bar beneath it, and the gap
/// above and below.
pub const OSD_LEVEL_HEIGHT: f32 = 2.0 * ui::tokens::Spacing::BASE.s3
    + ui::tokens::Metrics::BASE.icon_lg
    + 2.0 * ui::tokens::Spacing::BASE.s2;

/// Room above and below the pill's body for the shadow it floats on: a
/// surface sized to the body alone would cut the shadow off in a hard edge.
pub const OSD_MARGIN: f32 = ui::tokens::Spacing::BASE.s8;

/// How far the pill's edge stands above the output's bottom edge (or below
/// its top, when it moves up): the same inset the readouts have always rested
/// at, where a glance finds them and no bar or dock sits.
pub const OSD_INSET: f32 = ui::tokens::Spacing::BASE.s16;

/// The pill's content is at least this wide: a short word still leaves a bar
/// long enough to read as a level rather than a dash.
pub const OSD_BAR: f32 = ui::tokens::Metrics::BASE.icon_lg * 5.0;

/// How wide the pill's content grows, in glyphs, before it stops and cuts the
/// device's name with an ellipsis instead: a long device name widens the pill
/// to a point, never across the display.
pub const OSD_WIDEST: f32 = 12.0;

/// Where a readout stands on its display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// The bottom edge, where a glance finds it.
    #[default]
    Bottom,
    /// The top edge, when the bottom is where the person is typing.
    Top,
}

/// The height of the surface a host shows a readout on: the taller pill and
/// its shadow's room, in whole logical pixels.
#[must_use]
pub fn surface_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's height in logical pixels is a few dozen"
    )]
    let height = (OSD_LEVEL_HEIGHT.max(OSD_ROW_HEIGHT) + 2.0 * OSD_MARGIN).ceil() as i32;
    height
}

/// Where the readout's surface goes on the output `(x, y, w, h)`: a band
/// across the output, its pill centred in it, [`OSD_INSET`] in from the edge
/// `placement` names. The pill is as wide as what it reads, so the band is as
/// wide as the output and the pill centres itself in it.
#[must_use]
pub fn place(output: (i32, i32, i32, i32), placement: Placement) -> (i32, i32, i32, i32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "insets of a few dozen logical pixels"
    )]
    let (inset, margin) = (OSD_INSET.round() as i32, OSD_MARGIN.round() as i32);
    let (x, y, w, h) = output;
    let height = surface_height();
    let top = match placement {
        Placement::Bottom => y
            .saturating_add(h)
            .saturating_sub(inset)
            .saturating_add(margin)
            .saturating_sub(height),
        Placement::Top => y.saturating_add(inset).saturating_sub(margin),
    };
    (x, top, w.max(1), height)
}

/// How wide a pill may be before it is no longer "under the caret": the
/// longest a readout's content is drawn, around the middle of the display.
const READING_HALF_WIDTH: f64 = 240.0;

/// Where a readout on `output` stands so the text caret `caret` (in the same
/// desk coordinates) keeps clear of it: the bottom, unless the caret is in
/// the way there — typing at the bottom of the screen must not be covered by
/// what the person just changed.
#[must_use]
pub fn clear_of_caret(caret: Option<Rect>, output: (i32, i32, i32, i32)) -> Placement {
    let Some(caret) = caret else {
        return Placement::Bottom;
    };
    let (seen, half) = (f64::from(OSD_INSET), READING_HALF_WIDTH);
    let (x, y, w, h) = output;
    let (cx, bottom) = (
        f64::from(x) + f64::from(w) / 2.0,
        f64::from(y) + f64::from(h),
    );
    let band = Rect::new(
        cx - half,
        bottom - seen - f64::from(surface_height()),
        cx + half,
        bottom - seen,
    );
    let overlaps =
        caret.x1 > band.x0 && caret.x0 < band.x1 && caret.y1 > band.y0 && caret.y0 < band.y1;
    if overlaps {
        Placement::Top
    } else {
        Placement::Bottom
    }
}

/// What a readout is about.
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
    /// The night light was turned on or off.
    NightLight,
    /// Do Not Disturb was turned on or off.
    DoNotDisturb,
    /// The screen magnifier was turned on or off.
    Magnifier,
    /// The keys are being taken for something else (a shortcut's chord).
    CapturedKeys,
    /// The pointer was let go where it was held.
    PointerReleased,
    /// The keyboard's input source changed: a layout, or a composing input
    /// method.
    InputSource,
}

impl OsdKind {
    /// Every kind, as its published name spells it.
    pub const ALL: [Self; 18] = [
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
        Self::NightLight,
        Self::DoNotDisturb,
        Self::Magnifier,
        Self::CapturedKeys,
        Self::PointerReleased,
        Self::InputSource,
    ];

    /// Whether the readout is a level with a bar, rather than a state said in
    /// words.
    #[must_use]
    pub const fn has_level(self) -> bool {
        matches!(
            self,
            Self::Volume | Self::Brightness | Self::KeyboardBacklight
        )
    }
}

/// Which device a reading is about, and what to draw for it: the glyph says
/// *what* changed (a pair of headphones, a panel), so the words can say which
/// one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceGlyph {
    /// A speaker, the machine's own or a pair of them.
    #[default]
    Speaker,
    Headphones,
    /// A display: a panel or a monitor on a cable.
    Display,
    Keyboard,
}

impl DeviceGlyph {
    /// The glyph a sink called `name` plays through: headphones and headsets
    /// say so in their name, everything else is a speaker.
    #[must_use]
    pub fn of_sink(name: &str) -> Self {
        let name = name.to_lowercase();
        if ["headphone", "headset", "earbud", "buds", "airpods", "beats"]
            .iter()
            .any(|word| name.contains(word))
        {
            Self::Headphones
        } else {
            Self::Speaker
        }
    }

    const fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Speaker => ui::icons::Icon::Speaker,
            Self::Headphones => ui::icons::Icon::Headphones,
            Self::Display => ui::icons::Icon::Monitor,
            Self::Keyboard => ui::icons::Icon::Keyboard,
        }
    }
}

/// The device a reading is about: what the person knows it by, and its glyph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub glyph: DeviceGlyph,
}

impl Device {
    #[must_use]
    pub fn new(name: impl Into<String>, glyph: DeviceGlyph) -> Self {
        Self {
            name: name.into(),
            glyph,
        }
    }
}

/// One readout: either an amount read as a level, or a state said in words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shows", content = "reading", rename_all = "kebab-case")]
pub enum Osd {
    Level(Level),
    Hint(Hint),
}

/// A reading with a bar: the volume, a display's brightness, the keyboard's
/// backlight. `level` runs `0.0 ..= 1.0`; `on` is false when what it reads is
/// muted or off, which fills nothing while the bar keeps its place.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub kind: OsdKind,
    #[serde(default)]
    pub level: f32,
    #[serde(default = "on")]
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
}

/// A mode change said in words ("Night light on"), with the chord that did it
/// where there was one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    pub kind: OsdKind,
    #[serde(default = "on")]
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord: Option<String>,
    /// What the person calls what changed, where the kind alone does not say
    /// it: the name of the input source that now types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

const fn on() -> bool {
    true
}

impl From<Level> for Osd {
    fn from(level: Level) -> Self {
        Self::Level(level)
    }
}

impl From<Hint> for Osd {
    fn from(hint: Hint) -> Self {
        Self::Hint(hint)
    }
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

/// What the person calls the kind of reading when no device is named.
fn kind_name(kind: OsdKind) -> String {
    match kind {
        OsdKind::Volume => t("osd-volume-name"),
        OsdKind::Brightness => t("osd-brightness-name"),
        OsdKind::KeyboardBacklight => t("osd-keyboard-backlight-name"),
        _ => t("osd-readout"),
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

impl Level {
    /// The whole reading, as one line for a person who cannot see it.
    #[must_use]
    pub fn caption(&self) -> String {
        t_args(
            "osd-reading",
            &[
                ("device", self.device_name().into()),
                ("value", self.value_text().into()),
            ],
        )
    }

    /// The short value the pill reads: `65%`, `Muted`, `Off`.
    #[must_use]
    pub fn value_text(&self) -> String {
        let percent = || t_args("osd-percent", &[("percent", percent(self.level).into())]);
        match self.kind {
            OsdKind::Volume if !self.on => t("osd-muted"),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-off"),
            _ => percent(),
        }
    }

    /// What the pill calls the thing it read: the device's name, else the
    /// kind's.
    #[must_use]
    pub fn device_name(&self) -> String {
        self.device
            .as_ref()
            .map_or_else(|| kind_name(self.kind), |device| device.name.clone())
    }

    /// The glyph for the device it changed (its own when the session named
    /// it), and whether it reads as on.
    #[must_use]
    pub fn glyph(&self) -> (ui::icons::Icon, GlyphTone) {
        use ui::icons::Icon;
        if let Some(device) = &self.device {
            let tone = if self.kind == OsdKind::KeyboardBacklight && self.level <= 0.0 {
                GlyphTone::Off
            } else {
                GlyphTone::On
            };
            return (device.glyph.icon(), tone);
        }
        match self.kind {
            OsdKind::Volume if self.on => (Icon::Volume2, GlyphTone::On),
            OsdKind::Volume => (Icon::VolumeX, GlyphTone::On),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => (Icon::Keyboard, GlyphTone::Off),
            OsdKind::CapturedKeys | OsdKind::KeyboardBacklight => (Icon::Keyboard, GlyphTone::On),
            _ => (Icon::Sun, GlyphTone::On),
        }
    }

    /// How much of the level bar is filled: a muted volume keeps its bar and
    /// empties its fill, so the pill does not change shape while still
    /// agreeing with the crossed-out volume glyph.
    #[must_use]
    pub fn filled(&self) -> f64 {
        if self.kind == OsdKind::Volume && !self.on {
            0.0
        } else {
            f64::from(self.level.clamp(0.0, 1.0))
        }
    }
}

impl Hint {
    /// The words the pill reads: what the mode is, and where it now stands.
    #[must_use]
    pub fn words(&self) -> String {
        match self.kind {
            OsdKind::NightLight if self.on => t("osd-night-light-on"),
            OsdKind::NightLight => t("osd-night-light-off"),
            OsdKind::DoNotDisturb if self.on => t("osd-do-not-disturb-on"),
            OsdKind::DoNotDisturb => t("osd-do-not-disturb-off"),
            OsdKind::Magnifier if self.on => t("osd-magnifier-on"),
            OsdKind::Magnifier => t("osd-magnifier-off"),
            OsdKind::CapturedKeys => t("osd-keys-captured"),
            OsdKind::InputSource => self.name.clone().unwrap_or_else(|| t("osd-input-source")),
            OsdKind::PointerReleased => t("osd-pointer-released"),
            OsdKind::Microphone if self.on => t("osd-microphone-on"),
            OsdKind::Microphone => t("osd-microphone-muted"),
            OsdKind::AirplaneMode if self.on => t("osd-airplane-mode-on"),
            OsdKind::AirplaneMode => t("osd-airplane-mode-off"),
            OsdKind::Touchpad if self.on => t("osd-touchpad-on"),
            OsdKind::Touchpad => t("osd-touchpad-off"),
            // What the transport did, and the song it did it to: a pause names
            // what stopped, which the words alone cannot. The catalogue says
            // both as one sentence, so the pill stays a single line of text.
            kind @ (OsdKind::Play | OsdKind::Pause) => {
                let named = if kind == OsdKind::Play {
                    "osd-play-named"
                } else {
                    "osd-pause-named"
                };
                match &self.name {
                    Some(name) if !name.is_empty() => {
                        t_args(named, &[("song", name.clone().into())])
                    }
                    _ if kind == OsdKind::Play => t("osd-play"),
                    _ => t("osd-pause"),
                }
            }
            OsdKind::NextTrack => t("osd-next-track"),
            OsdKind::PreviousTrack => t("osd-previous-track"),
            OsdKind::Stop => t("osd-stop"),
            OsdKind::NoBacklight => t("osd-no-backlight"),
            OsdKind::Volume | OsdKind::Brightness | OsdKind::KeyboardBacklight => {
                kind_name(self.kind)
            }
        }
    }

    /// The glyph for what was turned on or off, and whether it reads as on.
    #[must_use]
    pub fn glyph(&self) -> (ui::icons::Icon, GlyphTone) {
        use ui::icons::Icon;
        match self.kind {
            OsdKind::NightLight if self.on => (Icon::Moon, GlyphTone::On),
            OsdKind::NightLight => (Icon::Moon, GlyphTone::Off),
            OsdKind::DoNotDisturb if self.on => (Icon::BellOff, GlyphTone::On),
            OsdKind::DoNotDisturb => (Icon::Bell, GlyphTone::Off),
            OsdKind::Magnifier if self.on => (Icon::ZoomIn, GlyphTone::On),
            OsdKind::Magnifier => (Icon::ZoomIn, GlyphTone::Off),
            OsdKind::PointerReleased => (Icon::Hand, GlyphTone::On),
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
            OsdKind::Volume => (Icon::Volume2, GlyphTone::On),
            OsdKind::Brightness => (Icon::Sun, GlyphTone::On),
            OsdKind::KeyboardBacklight | OsdKind::CapturedKeys | OsdKind::InputSource => {
                (Icon::Keyboard, GlyphTone::On)
            }
        }
    }
}

impl Osd {
    /// The line a person who cannot see the pill is told.
    #[must_use]
    pub fn caption(&self) -> String {
        match self {
            Self::Level(level) => level.caption(),
            Self::Hint(hint) => hint.words(),
        }
    }

    /// The glyph the pill leads with, and how it is inked.
    #[must_use]
    pub fn glyph(&self) -> (ui::icons::Icon, GlyphTone) {
        match self {
            Self::Level(level) => level.glyph(),
            Self::Hint(hint) => hint.glyph(),
        }
    }

    /// The reading's `kind`.
    #[must_use]
    pub fn kind(&self) -> OsdKind {
        match self {
            Self::Level(level) => level.kind,
            Self::Hint(hint) => hint.kind,
        }
    }

    /// The action's flat arguments for this reading, on `display` (the
    /// connector of the display that changed), as any caller would pass them.
    #[must_use]
    pub fn arguments(&self, display: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "kind".into(),
            serde_json::to_value(self.kind()).unwrap_or_default(),
        );
        match self {
            Self::Level(level) => {
                arguments.insert("level".into(), f64::from(level.level).into());
                arguments.insert("on".into(), level.on.into());
                if let Some(device) = &level.device {
                    arguments.insert("device".into(), device.name.clone().into());
                    arguments.insert(
                        "device-glyph".into(),
                        serde_json::to_value(device.glyph).unwrap_or_default(),
                    );
                }
            }
            Self::Hint(hint) => {
                arguments.insert("on".into(), hint.on.into());
                if let Some(chord) = &hint.chord {
                    arguments.insert("chord".into(), chord.clone().into());
                }
            }
        }
        if let Some(display) = display {
            arguments.insert("display".into(), display.into());
        }
        arguments
    }
}

/// The text styles the pill sets its lines in.
struct Styles {
    /// The device's name, above the bar.
    name: TextStyle,
    /// The value at the bar's end.
    value: TextStyle,
    /// A hint's words.
    words: TextStyle,
    /// The chord that asked for it.
    chord: TextStyle,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        Self {
            name: theme.text(TextRole::Label).with_weight(TextWeight::Medium),
            value: theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold)
                .with_kind(TextKind::Numeric),
            words: theme.text(TextRole::Label).with_weight(TextWeight::Medium),
            chord: theme.text(TextRole::Caption),
        }
    }
}

/// The pill's geometry: its glyph, the device's name, the level as a bar and
/// the value, or a hint's words and chord. Kept apart from paint so the rhythm
/// is testable and every reading, level or state, shares one shape.
#[derive(Clone, Debug, PartialEq)]
struct Pill {
    rect: Rect,
    glyph: Rect,
    /// The device's name (a level) or the hint's words.
    name: Rect,
    /// The level bar under the name, spanning the content: the level *is* the
    /// bar.
    bar: Option<Rect>,
    /// The value at the end of the name's row, for a level.
    value: Option<Rect>,
    name_text: String,
    value_text: String,
    chord: Option<(String, Rect)>,
}

/// What the pill reads, measured: the name or the hint's words, the value,
/// and the chord that asked for it where one did.
struct Words {
    name: String,
    name_w: f64,
    name_h: f64,
    value: String,
    /// The column a level's value takes: as wide as the widest reading, so the
    /// number gaining a digit never moves the name beside it.
    slot: f64,
    /// The width the widest line needs: what the bar spans and the pill is as
    /// wide as.
    content: f64,
    chord: Option<(String, f64, f64)>,
}

impl Words {
    /// `osd`'s lines in `styles`, measured: a level's device and value above a
    /// bar as wide as the widest reading, or a hint's words and its chord.
    fn of(osd: &Osd, styles: &Styles, text: &mut TextRenderer, gap: f64) -> Self {
        let (name_text, value_text, chord_text) = match osd {
            Osd::Level(level) => (level.device_name(), level.value_text(), None),
            Osd::Hint(hint) => (hint.words(), String::new(), hint.chord.clone()),
        };
        let name_style = match osd {
            Osd::Level(_) => &styles.name,
            Osd::Hint(_) => &styles.words,
        };
        let (name_w, name_h) = text.measure_style(&name_text, None, name_style);
        let (value_w, _) = text.measure_style(&value_text, None, &styles.value);
        let mut content = f64::from(name_w);
        let chord = chord_text.map(|words| {
            let (w, h) = text.measure_style(&words, None, &styles.chord);
            content += gap / 2.0 + f64::from(w);
            (words, f64::from(w), f64::from(h))
        });
        // A level's value stands in a column of its own, a gap after the
        // device's name: the two never share a run of pixels, and the name is
        // what gives way when there is no room for both.
        let mut slot = 0.0;
        if matches!(osd, Osd::Level(_)) {
            slot = f64::from(value_w);
            for reading in [
                t_args("osd-percent", &[("percent", 100.into())]),
                t("osd-muted"),
                t("osd-off"),
            ] {
                let (w, _) = text.measure_style(&reading, None, &styles.value);
                slot = slot.max(f64::from(w));
            }
            content = (f64::from(name_w) + gap + slot).max(f64::from(OSD_BAR));
        }
        Self {
            name: name_text,
            name_w: f64::from(name_w),
            name_h: f64::from(name_h),
            value: value_text,
            slot,
            content,
            chord,
        }
    }
}

impl Pill {
    /// Lay the pill out centred in the surface's `band`.
    fn layout(band: Rect, theme: &Theme, text: &mut TextRenderer, osd: &Osd) -> Self {
        let sp = &theme.tokens.spacing;
        let styles = Styles::new(theme);
        let icon = f64::from(theme.tokens.metrics.icon_lg);
        let (pad, gap, vpad) = (f64::from(sp.s4), f64::from(sp.s3), f64::from(sp.s3));
        let bar_gap = f64::from(sp.s2);
        let bar_h = f64::from(sp.s2).max(6.0);
        let words = Words::of(osd, &styles, text, gap);
        let content = words.content.min(icon * f64::from(OSD_WIDEST));
        let width = (pad * 2.0 + icon + gap + content).min(band.width());
        // Two rows for a level (the device and the value above the bar), one
        // for a hint, never taller than the band has room for.
        let levels = matches!(osd, Osd::Level(_));
        let body = if levels {
            icon.max(words.name_h) + bar_gap + bar_h
        } else {
            icon.max(words.name_h)
        };
        let room = (band.height() - 2.0 * f64::from(OSD_MARGIN)).max(0.0);
        let height = (body + 2.0 * vpad).min(room);
        // Bottom-centred: the pill grows upward from the line it rests on.
        let x0 = band.center().x - width / 2.0;
        let y1 = band.y1 - f64::from(OSD_MARGIN);
        let rect = Rect::new(x0, y1 - height, x0 + width, y1);
        let glyph = Rect::new(
            rect.x0 + pad,
            rect.center().y - icon / 2.0,
            rect.x0 + pad + icon,
            rect.center().y + icon / 2.0,
        );
        let content_x0 = glyph.x1 + gap;
        let content_x1 = (rect.x1 - pad).max(content_x0);
        let row = if levels {
            Rect::new(
                content_x0,
                rect.y0 + vpad,
                content_x1,
                rect.y0 + vpad + icon.max(words.name_h),
            )
        } else {
            Rect::new(content_x0, rect.y0 + vpad, content_x1, rect.y1 - vpad)
        };
        let middle = row.center().y;
        // The value keeps its own column at the row's end; the name stops a
        // gap short of it, so a long device name is cut with an ellipsis
        // rather than running into the reading.
        let slot = if levels {
            words.slot.min((row.x1 - row.x0) / 2.0)
        } else {
            0.0
        };
        let name_x1 = if levels {
            (row.x1 - slot - gap).max(row.x0)
        } else {
            row.x1
        };
        let name = Rect::new(
            row.x0,
            middle - words.name_h / 2.0,
            name_x1,
            middle + words.name_h / 2.0,
        );
        let bar = levels.then(|| {
            Rect::new(
                content_x0,
                rect.y1 - vpad - bar_h,
                content_x1,
                rect.y1 - vpad,
            )
        });
        let value = levels.then(|| Rect::new(content_x1 - slot, name.y0, content_x1, name.y1));
        let chord = words.chord.map(|(chord, w, h)| {
            let x0 = name.x0 + words.name_w + gap / 2.0;
            (
                chord,
                Rect::new(x0, middle - h / 2.0, x0 + w, middle + h / 2.0),
            )
        });
        Self {
            rect,
            glyph,
            name,
            bar,
            value,
            name_text: words.name,
            value_text: words.value,
            chord,
        }
    }
}

/// The empty part of the level bar: a hairline wash over the pill's own
/// ground, so it reads on every palette rather than vanishing under a lift
/// of the desktop's base.
fn bar_color(theme: &Theme) -> vello::peniko::Color {
    ui::tokens::colors::Colors::hairline_on(ui::chrome::shell::panel_ground(theme), 80)
}

/// Paint the readout's pill in `band`, `level` of the bar filled, returning
/// its resting rect.
fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    raised: &Osd,
    level: f64,
) -> Rect {
    let pill = Pill::layout(band, theme, text, raised);
    let painted = ui::chrome::shell::paint_panel(scene, transform, pill.rect, theme, 1.0);
    let theme = &painted.theme;
    draw_glyph(scene, transform, &pill, theme, raised);
    draw_name(scene, transform, text, &pill, theme, raised);
    draw_bar(scene, transform, &pill, theme, level);
    draw_value(scene, transform, text, &pill, theme, raised);
    pill.rect
}

/// Paint the readout's glyph, inked for what it reads: lit, dimmed when the
/// thing is off, warning where it asks for the person's attention.
fn draw_glyph(scene: &mut Scene, transform: Affine, pill: &Pill, theme: &Theme, raised: &Osd) {
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
}

/// Paint the pill's name (a level's device, a hint's words), and the chord
/// that asked for it where one did.
fn draw_name(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    pill: &Pill,
    theme: &Theme,
    raised: &Osd,
) {
    let styles = Styles::new(theme);
    let colors = &theme.tokens.colors;
    let (name_style, name_tint) = match raised {
        Osd::Level(_) => (&styles.name, colors.text_dim),
        Osd::Hint(_) => (&styles.words, colors.text_primary),
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's name room in logical pixels is far inside f32 range"
    )]
    let name_room = pill.name.width() as f32;
    let name = text.elide_to_width_style(&pill.name_text, name_room, name_style);
    let (_, name_h) = text.measure_style(&name, None, name_style);
    text.draw_style(
        scene,
        &name,
        name_tint,
        transform
            .pre_translate((pill.name.x0, pill.name.center().y - f64::from(name_h) / 2.0).into()),
        None,
        name_style,
    );
    let Some((chord, chord_rect)) = &pill.chord else {
        return;
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a chord's room in logical pixels is far inside f32 range"
    )]
    let chord_room = chord_rect.width() as f32;
    let shown = text.elide_to_width_style(chord, chord_room, &styles.chord);
    let (_, chord_h) = text.measure_style(&shown, None, &styles.chord);
    text.draw_style(
        scene,
        &shown,
        colors.text_dim,
        transform.pre_translate(
            (
                chord_rect.x0,
                chord_rect.center().y - f64::from(chord_h) / 2.0,
            )
                .into(),
        ),
        None,
        &styles.chord,
    );
}

/// Paint the level as a bar: its empty track, and the filled share of it.
fn draw_bar(scene: &mut Scene, transform: Affine, pill: &Pill, theme: &Theme, level: f64) {
    let Some(bar) = pill.bar else {
        return;
    };
    let bar_h = bar.height();
    scene.fill(
        Fill::NonZero,
        transform,
        bar_color(theme),
        None,
        &vello::kurbo::RoundedRect::from_rect(bar, bar_h / 2.0),
    );
    if level > 0.0 {
        let w = (bar.width() * level).max(bar_h);
        let fill = Rect::new(bar.x0, bar.y0, bar.x0 + w, bar.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.text_primary,
            None,
            &vello::kurbo::RoundedRect::from_rect(fill, bar_h / 2.0),
        );
    }
}

/// Paint the value at the end of the bar's row, dimmed where it stands for
/// nothing (a muted output).
fn draw_value(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    pill: &Pill,
    theme: &Theme,
    raised: &Osd,
) {
    let Some(value) = pill.value else {
        return;
    };
    let styles = Styles::new(theme);
    let colors = &theme.tokens.colors;
    let muted = matches!(
        raised,
        Osd::Level(Level {
            kind: OsdKind::Volume,
            on: false,
            ..
        })
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a value's room in logical pixels is far inside f32 range"
    )]
    let value_room = value.width() as f32;
    let shown = text.elide_to_width_style(&pill.value_text, value_room, &styles.value);
    let (shown_w, shown_h) = text.measure_style(&shown, None, &styles.value);
    text.draw_style(
        scene,
        &shown,
        if muted {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate(
            (
                value.x1 - f64::from(shown_w),
                value.center().y - f64::from(shown_h) / 2.0,
            )
                .into(),
        ),
        None,
        &styles.value,
    );
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
            ActionParam::optional_owned(
                "device",
                ParamType::Text,
                t("desktop-action-param-osd-device"),
            ),
            ActionParam::optional_owned(
                "device-glyph",
                ParamType::Text,
                t("desktop-action-param-osd-device-glyph"),
            ),
            ActionParam::optional_owned(
                "chord",
                ParamType::Text,
                t("desktop-action-param-osd-chord"),
            ),
            ActionParam::optional_owned(
                "display",
                ParamType::Text,
                t("desktop-action-param-osd-display"),
            ),
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
            let on = args.boolean("on").unwrap_or(true);
            let osd = if kind.has_level() {
                Osd::from(Level {
                    kind,
                    level: level.clamp(0.0, 1.0),
                    on,
                    device: args.text("device").map(|name| Device {
                        name: name.into(),
                        glyph: args
                            .text("device-glyph")
                            .and_then(|glyph| {
                                serde_json::from_value::<DeviceGlyph>(serde_json::Value::String(
                                    glyph.into(),
                                ))
                                .ok()
                            })
                            .unwrap_or_else(|| DeviceGlyph::of_sink(name)),
                    }),
                })
            } else {
                Osd::from(Hint {
                    kind,
                    on,
                    chord: args.text("chord").map(Into::into),
                    name: args.text("name").map(Into::into),
                })
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

/// The readout up now: what it says, when it came, when it last changed, how
/// open it is drawn and where its bar stands.
struct Raised {
    osd: Osd,
    /// When this run of readouts appeared: a burst of presses keeps it.
    shown_since: Instant,
    /// When it last changed: it stays [`OSD_HOLD`] after.
    changed_at: Instant,
    /// Its coming and going, on the readout's motion: a new reading mid-flight
    /// keeps the opener, and a hide it reverses starts from where it is drawn.
    presence: Presence,
    /// The bar's fill, on a spring: it follows the reading rather than
    /// jumping to it, and a reversed press turns from where it is.
    level: MotionValue<f64>,
}

/// The slab, as a widget filling the rectangle its host gives it.
pub struct OsdSlab {
    raised: Option<Raised>,
    rect: Rect,
    /// Whether the person asked for no motion, as the last theme drew it.
    reduce_motion: bool,
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
            reduce_motion: false,
        }
    }

    /// Show `osd` now: a readout already up changes in place and stays up
    /// longer.
    pub fn show(&mut self, osd: Osd) {
        let now = ui::time::now();
        let motion = self.motion();
        let prior = self.raised.take().filter(|raised| live(raised, now));
        let shown_since = prior.as_ref().map_or(now, |raised| raised.shown_since);
        let same = prior
            .as_ref()
            .is_some_and(|raised| same_target(&raised.osd, &osd));
        let target = match &osd {
            Osd::Level(level) => level.filled(),
            Osd::Hint(_) => 0.0,
        };
        let mut presence = prior
            .as_ref()
            .map_or_else(Presence::closed, |raised| raised.presence);
        presence.show(true, Transient::READOUT, motion, MotionSource::Pointer, now);
        // A new kind is another thing said: its bar starts where the reading
        // is. The same reading pressed again springs from where it stands.
        let mut bar = prior
            .as_ref()
            .filter(|_| same)
            .map_or_else(|| MotionValue::new(target), |raised| raised.level);
        if same {
            let timing = Transient::READOUT.timing(Direction::Enter, motion, MotionSource::Pointer);
            bar.set(target, timing, now);
        } else {
            bar.snap(target);
        }
        self.raised = Some(Raised {
            osd,
            shown_since,
            changed_at: now,
            presence,
            level: bar,
        });
    }

    /// The readout up now, if any.
    #[must_use]
    pub fn current(&self) -> Option<Osd> {
        let now = ui::time::now();
        self.raised
            .as_ref()
            .filter(|raised| live(raised, now))
            .map(|raised| raised.osd.clone())
    }

    /// How open the readout is drawn at `now`, `0..=1`: its show and hide are
    /// one interruptible spring, never a fresh fade.
    #[cfg(test)]
    #[must_use]
    fn openness_at(&self, now: Instant) -> f64 {
        self.raised
            .as_ref()
            .map_or(0.0, |raised| raised.presence.progress(now))
    }

    /// How much of the bar is filled at `now`.
    #[cfg(test)]
    #[must_use]
    fn level_at(&self, now: Instant) -> f64 {
        self.raised
            .as_ref()
            .map_or(0.0, |raised| raised.level.get(now).clamp(0.0, 1.0))
    }

    /// The motion the person allows.
    fn motion(&self) -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), self.reduce_motion)
    }

    /// Take the readout's hold into account at `now`: once it is over, ask it
    /// to go, and let it go once its spring has closed it.
    fn follow(&mut self, now: Instant) {
        let motion = self.motion();
        let Some(raised) = &mut self.raised else {
            return;
        };
        if now >= expires(raised) {
            raised.presence.show(
                false,
                Transient::READOUT,
                motion,
                MotionSource::Pointer,
                now,
            );
        }
        if !raised.presence.is_visible(now) {
            self.raised = None;
        }
    }
}

/// Whether both readings are of the same thing: a level follows on from one
/// for the same device, and starts afresh for another.
fn same_target(before: &Osd, after: &Osd) -> bool {
    match (before, after) {
        (Osd::Level(before), Osd::Level(after)) => {
            before.kind == after.kind
                && before.device.as_ref().map(|device| &device.name)
                    == after.device.as_ref().map(|device| &device.name)
        }
        (Osd::Hint(before), Osd::Hint(after)) => before.kind == after.kind,
        _ => false,
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
        self.reduce_motion = theme.reduce_motion;
        let now = ui::time::now();
        self.follow(now);
        let Some(raised) = self.raised.as_ref() else {
            return;
        };
        let openness = raised.presence.progress(now);
        let alpha = ui::motion::unit_opacity(openness);
        if alpha <= 0.0 {
            return;
        }
        // The pill rises into place as it comes and sinks as it goes: the
        // slide follows the same spring, so a hide reversed mid-flight
        // reverses from where the pill is drawn.
        let slide = if theme.reduce_motion {
            0.0
        } else {
            f64::from(theme.tokens.spacing.s2) * (1.0 - openness.clamp(0.0, 1.0))
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
        let Some(raised) = self.raised.as_ref() else {
            return;
        };
        let level = raised.level.get(now);
        draw_pill(scene, transform, text, theme, self.rect, &raised.osd, level);
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
        let now = ui::time::now();
        self.raised
            .as_ref()
            .is_some_and(|raised| live(raised, now) || raised.presence.is_visible(now))
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

    fn volume(level: f32) -> Osd {
        Osd::from(Level {
            kind: OsdKind::Volume,
            level,
            on: true,
            device: Some(Device::new("Studio headphones", DeviceGlyph::Headphones)),
        })
    }

    fn brightness(level: f32) -> Osd {
        Osd::from(Level {
            kind: OsdKind::Brightness,
            level,
            on: true,
            device: Some(Device::new("Built-in display", DeviceGlyph::Display)),
        })
    }

    fn night_light(on: bool) -> Osd {
        Osd::from(Hint {
            kind: OsdKind::NightLight,
            on,
            chord: Some("⌃⌥N".into()),
            name: None,
        })
    }

    /// A 1280 by 800 output's readout band.
    fn band() -> Rect {
        let (x, y, w, h) = place((0, 0, 1280, 800), Placement::Bottom);
        Rect::new(
            f64::from(x),
            f64::from(y),
            f64::from(x.saturating_add(w)),
            f64::from(y.saturating_add(h)),
        )
    }

    /// The pill `osd` lays out in the band of a 1280 by 800 output.
    fn pill(osd: &Osd) -> Pill {
        Pill::layout(band(), &Theme::dark(), &mut TextRenderer::new(), osd)
    }

    #[test]
    fn a_level_names_its_device_and_the_level_is_the_bar() {
        ui::i18n::I18nManager::init("en-US");
        let pill = pill(&volume(0.65));
        // The device is named above the bar, with the device's own glyph.
        assert_eq!(pill.name_text, "Studio headphones");
        assert_eq!(
            volume(0.65).glyph(),
            (ui::icons::Icon::Headphones, GlyphTone::On)
        );
        assert_eq!(pill.value_text, "65%");
        // The bar spans the content under the name, and the value ends it.
        let bar = pill.bar.expect("a level has a bar");
        let value = pill.value.expect("a level has a value");
        assert_eq!(bar.width(), value.x1 - bar.x0);
        assert!(bar.y0 > pill.name.y1, "the bar stands under the device");
        assert!(bar.height() >= 6.0, "a bar, not a hairline");
        // The glyph leads, a square, and the content follows it.
        assert_eq!(pill.glyph.x0, pill.rect.x0 + 16.0);
        assert_eq!(pill.glyph.width(), pill.glyph.height());
        assert!(pill.name.x0 > pill.glyph.x1);
        // A level's pill is taller than a hint's, and rests on the same line.
        assert!(pill.rect.height() > f64::from(OSD_ROW_HEIGHT));
        assert_eq!(pill.rect.y1, band().y1 - f64::from(OSD_MARGIN));
        assert_eq!(pill.rect.center().x, 640.0);
        assert_eq!(
            volume(0.65).caption(),
            "Studio headphones, 65%",
            "a person who cannot see it hears the device and the level"
        );
    }

    #[test]
    fn the_pill_is_as_wide_as_what_it_reads() {
        ui::i18n::I18nManager::init("en-US");
        let level = |name: &str, glyph| {
            Osd::from(Level {
                kind: OsdKind::Volume,
                level: 0.1,
                on: true,
                device: Some(Device::new(name, glyph)),
            })
        };
        let short = level("Buds", DeviceGlyph::Headphones);
        let long = level("Studio monitoring headphones", DeviceGlyph::Headphones);
        assert!(
            pill(&long).rect.width() > pill(&short).rect.width(),
            "a longer device name makes a wider pill"
        );
        // Its reading may change without the pill changing shape: the value
        // slot fits the widest reading, so the bar keeps its length.
        let wide = pill(&volume(0.65)).rect.width();
        let muted = Osd::from(Level {
            kind: OsdKind::Volume,
            level: 0.65,
            on: false,
            device: Some(Device::new("Studio headphones", DeviceGlyph::Headphones)),
        });
        for other in [volume(0.05), volume(1.0), muted] {
            assert_eq!(
                pill(&other).rect.width(),
                wide,
                "{other:?} resized the pill"
            );
        }
        // A hint is one row, and narrows to its words.
        let hint = pill(&night_light(true));
        assert!(hint.bar.is_none());
        assert!(hint.rect.height() < pill(&volume(0.65)).rect.height());
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's text room in logical pixels is far inside f32 range"
    )]
    fn the_device_name_never_runs_into_the_value() {
        ui::i18n::I18nManager::init("en-US");
        let named = |name: &str| {
            Osd::from(Level {
                kind: OsdKind::Volume,
                level: 0.65,
                on: true,
                device: Some(Device::new(name, DeviceGlyph::Headphones)),
            })
        };
        let theme = Theme::dark();
        let styles = Styles::new(&theme);
        let mut text = TextRenderer::new();
        for osd in [
            volume(0.65),
            named("Studio monitoring headphones over there on the desk"),
            named("Buds"),
            brightness(0.4),
        ] {
            let pill = Pill::layout(band(), &theme, &mut text, &osd);
            let value = pill.value.expect("a level has a value");
            let name =
                text.elide_to_width_style(&pill.name_text, pill.name.width() as f32, &styles.name);
            let (name_w, _) = text.measure_style(&name, None, &styles.name);
            let shown =
                text.elide_to_width_style(&pill.value_text, value.width() as f32, &styles.value);
            let (value_w, _) = text.measure_style(&shown, None, &styles.value);
            // The two runs of words, where they are drawn: apart, never over
            // each other, whatever the device is called.
            let name_run = pill.name.x0..pill.name.x0 + f64::from(name_w);
            let value_run = value.x1 - f64::from(value_w)..value.x1;
            assert!(
                name_run.end <= value_run.start,
                "{}: the device's name runs into the value: {name_run:?} {value_run:?}",
                pill.name_text
            );
        }
        // A name a person meets every day fits whole at a normal width, rather
        // than being cut to make room for a column the value could have kept.
        let everyday = Pill::layout(band(), &theme, &mut text, &volume(0.65));
        assert_eq!(
            text.elide_to_width_style(
                &everyday.name_text,
                everyday.name.width() as f32,
                &styles.name
            ),
            "Studio headphones",
            "the whole device name is shown"
        );
    }

    #[test]
    fn the_bar_empties_when_the_sound_is_muted() {
        let level = |level: f32, on: bool| Level {
            kind: OsdKind::Volume,
            level,
            on,
            device: None,
        };
        assert!((level(0.37, true).filled() - 0.37).abs() < 1e-6);
        assert_eq!(level(0.37, false).filled(), 0.0);
        assert_eq!(level(2.0, true).filled(), 1.0);
        assert_eq!(level(-1.0, true).filled(), 0.0);
        assert_eq!(level(0.37, false).value_text(), "Muted");
    }

    #[test]
    fn a_hint_says_the_mode_in_words_and_keeps_its_chord() {
        ui::i18n::I18nManager::init("en-US");
        let hint = night_light(true);
        assert_eq!(hint.caption(), "Night light on");
        let pill = pill(&hint);
        assert_eq!(pill.name_text, "Night light on");
        assert_eq!(
            pill.chord.as_ref().map(|(chord, _)| chord.as_str()),
            Some("⌃⌥N")
        );
        // Every mode a person cannot see says what changed.
        for (kind, on, words) in [
            (OsdKind::DoNotDisturb, true, "Do not disturb on"),
            (OsdKind::DoNotDisturb, false, "Do not disturb off"),
            (OsdKind::Magnifier, true, "Magnifier on"),
            (OsdKind::NightLight, false, "Night light off"),
            (OsdKind::CapturedKeys, true, "Keys captured"),
            (OsdKind::PointerReleased, true, "Pointer released"),
            (OsdKind::Touchpad, false, "Touchpad off"),
        ] {
            assert_eq!(
                Osd::from(Hint {
                    kind,
                    on,
                    chord: None,
                    name: None,
                })
                .caption(),
                words
            );
        }
    }

    #[test]
    fn a_transport_hint_names_the_song_it_acted_on() {
        ui::i18n::I18nManager::init("en-US");
        let named = |kind, song: Option<&str>| {
            Osd::from(Hint {
                kind,
                on: true,
                chord: None,
                name: song.map(str::to_owned),
            })
            .caption()
        };
        // A pause says what stopped, so the person knows which of the players
        // in the room went quiet; without a song it says what the key does.
        assert_eq!(
            named(OsdKind::Play, Some("Glasslight")),
            "Playing Glasslight"
        );
        assert_eq!(
            named(OsdKind::Pause, Some("Glasslight")),
            "Paused Glasslight"
        );
        assert_eq!(named(OsdKind::Play, None), "Play");
        assert_eq!(named(OsdKind::Pause, Some("")), "Pause");
    }

    #[test]
    fn every_kind_has_a_glyph_and_says_something() {
        ui::i18n::I18nManager::init("en-US");
        for kind in OsdKind::ALL {
            let hint = Osd::from(Hint {
                kind,
                on: true,
                chord: None,
                name: None,
            });
            let (_, tone) = hint.glyph();
            assert!(matches!(
                tone,
                GlyphTone::On | GlyphTone::Off | GlyphTone::Warn
            ));
            assert!(!hint.caption().is_empty(), "{kind:?} says nothing");
        }
        assert!(OsdKind::Volume.has_level());
        assert!(!OsdKind::NightLight.has_level());
        assert_eq!(
            DeviceGlyph::of_sink("Studio headphones"),
            DeviceGlyph::Headphones
        );
        assert_eq!(DeviceGlyph::of_sink("Built-in Audio"), DeviceGlyph::Speaker);
    }

    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "spacing steps are whole logical pixels"
    )]
    fn the_readout_moves_out_of_the_text_carets_way() {
        let output = (0, 0, 1280, 800);
        assert_eq!(clear_of_caret(None, output), Placement::Bottom);
        // Typing in a field near the bottom centre: the readout stands at the
        // top, clear of the line being typed.
        let caret = Rect::new(620.0, 700.0, 628.0, 720.0);
        assert_eq!(clear_of_caret(Some(caret), output), Placement::Top);
        let (x, y, w, _) = place(output, Placement::Top);
        assert_eq!(x, 0);
        assert_eq!(y, (OSD_INSET - OSD_MARGIN) as i32);
        assert_eq!(w, 1280);
        // A caret far from the middle is no concern: the pill stays where a
        // glance finds it.
        let far = Rect::new(40.0, 700.0, 48.0, 720.0);
        assert_eq!(clear_of_caret(Some(far), output), Placement::Bottom);
        // Nor is one in the middle of the screen, away from the foot.
        let middle = Rect::new(620.0, 300.0, 628.0, 320.0);
        assert_eq!(clear_of_caret(Some(middle), output), Placement::Bottom);
        // A second display is judged on its own foot.
        let second = (1280, 0, 1920, 1080);
        assert_eq!(clear_of_caret(Some(caret), second), Placement::Bottom);
    }

    #[test]
    fn a_readout_shows_by_its_action_changes_in_place_and_goes() {
        let _settled = ui::motion::testing::settled();
        ui::i18n::I18nManager::init("en-US");
        let mut slab = OsdSlab::new();
        assert!(slab.current().is_none());
        let resource = osd_resource();
        let spec = &resource.actions[0];
        let show = |arguments: &serde_json::Map<String, serde_json::Value>| {
            tilo_actions::door::encode_json(
                std::slice::from_ref(&resource),
                &tilo_actions::ActionAddress::new(OSD_RESOURCE, spec.id.as_str()),
                arguments,
            )
            .expect("the readout's action takes these arguments")
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("kind".into(), "brightness".into());
        arguments.insert("level".into(), 0.3.into());
        arguments.insert("device".into(), "Built-in display".into());
        arguments.insert("device-glyph".into(), "display".into());
        let encoded = show(&arguments);
        let osd: Osd = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(
            osd,
            brightness(0.3),
            "the action carries the device the key changed"
        );
        slab.show(osd);
        assert_eq!(
            slab.current().map(|osd| osd.kind()),
            Some(OsdKind::Brightness)
        );
        slab.show(brightness(0.4));
        match slab.current().expect("still up") {
            Osd::Level(level) => assert_eq!(level.level, 0.4),
            Osd::Hint(_) => panic!("a brightness is a level"),
        }
        // A mode change shows as a hint, with the chord that made it.
        let mut arguments = serde_json::Map::new();
        arguments.insert("kind".into(), "night-light".into());
        arguments.insert("chord".into(), "⌃⌥N".into());
        let encoded = show(&arguments);
        let osd: Osd = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(osd, night_light(true));
        slab.show(osd);
        assert_eq!(
            slab.current().map(|osd| osd.caption()),
            Some("Night light on".to_owned())
        );
    }

    #[test]
    fn the_level_follows_on_a_spring_and_a_reverse_turns_from_where_it_is() {
        ui::i18n::I18nManager::init("en-US");
        let mut slab = OsdSlab::new();
        slab.show(volume(0.2));
        let pressed = ui::time::now();
        slab.show(volume(0.8));
        // Thirty milliseconds after the second press the bar is on its way,
        // not already there: the level is a spring, never a jump.
        let early_at = pressed.checked_add(Duration::from_millis(30)).unwrap();
        let early = slab.level_at(early_at);
        assert!((0.2..0.8).contains(&early), "the bar moved to {early}");
        let later = pressed.checked_add(Duration::from_millis(60)).unwrap();
        assert!(slab.level_at(later) > early, "the bar keeps moving");
        let settled_at = pressed.checked_add(Duration::from_millis(600)).unwrap();
        assert!((slab.level_at(settled_at) - 0.8).abs() < 1e-3);
        assert!(slab.openness_at(early_at) > 0.0, "the readout is up");
        // Pressing it down again turns the bar back from where it is drawn,
        // never cutting to the new reading.
        let before = slab.level_at(ui::time::now());
        slab.show(volume(0.1));
        let after = ui::time::now()
            .checked_add(Duration::from_millis(2))
            .unwrap();
        assert!(before > 0.2, "the bar had started to rise: {before}");
        assert!(
            slab.level_at(after) > before - 0.05,
            "it turned from where it was drawn"
        );
    }

    #[test]
    fn a_readout_takes_itself_away_on_the_spring_that_brought_it() {
        ui::i18n::I18nManager::init("en-US");
        let mut slab = OsdSlab::new();
        slab.show(volume(0.5));
        let up = ui::time::now();
        assert!(slab.needs_redraw());
        assert!(slab.openness_at(up) > 0.0, "it is up");
        // Its hold runs, and then it goes: the openness falls on the same
        // spring the coming used, rather than being cut off.
        let leaving = up
            .checked_add(OSD_HOLD)
            .and_then(|at| at.checked_add(Duration::from_millis(20)))
            .unwrap();
        slab.follow(leaving);
        let falling = leaving.checked_add(Duration::from_millis(120)).unwrap();
        assert!(slab.openness_at(falling) < 0.5, "it is on its way out");
        let gone = leaving.checked_add(Duration::from_millis(600)).unwrap();
        slab.follow(gone);
        assert!(slab.raised.is_none(), "it has gone, and asks for no frame");
        assert!(!slab.needs_redraw());
    }
}
