//! The transient readout for the hardware keys: volume, brightness, the keyboard's backlight,
//! playback and the machine toggles.
//! A key that changes something invisible (the volume) or unreadable (the brightness, *because* it
//! is what changed) needs an answer on screen: a slab that appears, says **which device it
//! changed**, shows the new level as a bar, and gets out of the way. It never takes focus or the
//! pointer and never outlives the moment it was about. It is deliberately not a notification, which
//! persists and stacks: pressing volume up eight times shows one slab counting up, not eight
//! banners.
//! Two shapes, one action: [`Osd::Level`] for something read as an amount (a volume, a backlight)
//! and [`Osd::Hint`] for a mode whose new state words are enough (night light, Do Not Disturb, a
//! chord taken). Whoever handled the key shows it through its published action
//! (`desktop:osd/show`), as any caller would.

use serde::{Deserialize, Serialize};
use tilo_actions::ResourceSpec;
use tilo_resource::{offer, typed_resource};
use ui::time::Duration;
use ui::vello::kurbo::Rect;

use crate::desktop::i18n::{t, t_args};

/// The operation type the readout's action carries.
pub const OPERATION: &str = <OsdShow as tilo_resource::Effect>::OPERATION;
/// The readout's resource.
pub const OSD_RESOURCE: &str = "desktop:osd";

/// The readout as the session publishes it, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "desktop", label = "On-screen readout")]
pub struct Readout;

/// What shows a readout: one declared effect, answered by the slab that draws it.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.desktop.osd", catalogue = "../core/locales")]
pub enum OsdShow {
    /// Show a readout
    #[effect(kind = "undoable", label = "desktop-action-show-osd")]
    Show {
        /// What it is about: volume, brightness, a mode's new state
        #[param(
            "What it is about: volume, brightness, keyboard-backlight, microphone, airplane-mode, \
             touchpad, play, pause, next-track, previous-track, stop, no-backlight, night-light, \
             do-not-disturb, magnifier, captured-keys or pointer-released"
        )]
        kind: String,
        /// The level, from 0 to 1
        #[param("The level, from 0 to 1")]
        level: Option<f64>,
        /// Whether it is on
        #[param("Whether it is on")]
        on: Option<bool>,
        /// The device it changed, as a person knows it
        #[param("The device it changed, as a person knows it")]
        device: Option<String>,
        /// The device's glyph: speaker, headphones, display or keyboard
        #[param(
            name = "device-glyph",
            "The device's glyph: speaker, headphones, display or keyboard"
        )]
        device_glyph: Option<String>,
        /// The chord that asked for it
        #[param("The chord that asked for it")]
        chord: Option<String>,
        /// The connector of the display it shows on
        #[param("The connector of the display it shows on")]
        display: Option<String>,
    },
}

impl OsdShow {
    /// The readout this effect raises.
    #[must_use]
    pub fn osd(&self) -> Osd {
        let Self::Show {
            kind,
            level,
            on,
            device,
            device_glyph,
            chord,
            display: _,
        } = self;
        let kind = serde_json::from_value::<OsdKind>(serde_json::Value::String(kind.clone()))
            .unwrap_or(OsdKind::Volume);
        #[allow(clippy::cast_possible_truncation, reason = "a level between 0 and 1")]
        let level = level.unwrap_or(0.0) as f32;
        let on = on.unwrap_or(true);
        if kind.has_level() {
            Osd::from(Level {
                kind,
                level: level.clamp(0.0, 1.0),
                on,
                device: device.as_deref().map(|name| Device {
                    name: name.into(),
                    glyph: device_glyph
                        .as_deref()
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
                chord: chord.as_deref().map(Into::into),
                name: None,
            })
        }
    }
}

/// How long the readout stays up after the last change, fade included: long enough to read after
/// one tap, short enough that a burst of presses does not leave it hanging around.
pub const OSD_HOLD: Duration = Duration::from_millis(1_500);

/// The hint pill's height: its glyph's square and the gap above and below it. Unscaled `BASE`
/// values because a host sizes the surface from this before any theme is in hand.
pub const OSD_ROW_HEIGHT: f32 =
    ui::tokens::Metrics::BASE.icon_lg + 2.0 * ui::tokens::Spacing::BASE.s3;

/// The level pill's height: the device's row, the bar beneath it, and the gap above and below.
pub const OSD_LEVEL_HEIGHT: f32 = 2.0 * ui::tokens::Spacing::BASE.s3
    + ui::tokens::Metrics::BASE.icon_lg
    + 2.0 * ui::tokens::Spacing::BASE.s2;

/// Room above and below the pill's body for the shadow it floats on: a surface sized to the body
/// alone would cut the shadow off in a hard edge.
pub const OSD_MARGIN: f32 = ui::tokens::Spacing::BASE.s8;

/// How far the pill's edge stands above the output's bottom edge (or below its top, when it moves
/// up): the same inset the readouts have always rested at, where a glance finds them and no bar or
/// dock sits.
pub const OSD_INSET: f32 = ui::tokens::Spacing::BASE.s16;

/// The pill's content is at least this wide: a short word still leaves a bar long enough to read as
/// a level rather than a dash.
pub const OSD_BAR: f32 = ui::tokens::Metrics::BASE.icon_lg * 5.0;

/// How wide the pill's content grows, in glyphs, before it stops and cuts the device's name with an
/// ellipsis instead: a long device name widens the pill to a point, never across the display.
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

/// The height of the surface a host shows a readout on: the taller pill and its shadow's room, in
/// whole logical pixels.
#[must_use]
pub fn surface_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's height in logical pixels is a few dozen"
    )]
    let height = (OSD_LEVEL_HEIGHT.max(OSD_ROW_HEIGHT) + 2.0 * OSD_MARGIN).ceil() as i32;
    height
}

/// Where the readout's surface goes on the output `(x, y, w, h)` : a band across the output, its
/// pill centred in it, [`OSD_INSET`] in from the edge `placement` names. The pill is as wide as
/// what it reads, so the band is as wide as the output and the pill centres itself in it.
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

/// How wide a pill may be before it is no longer "under the caret": the longest a readout's content
/// is drawn, around the middle of the display.
const READING_HALF_WIDTH: f64 = 240.0;

/// Where a readout on `output` stands so the text caret `caret` (in the same shell coordinates)
/// keeps clear of it: the bottom, unless the caret is in the way there — typing at the bottom of
/// the screen must not be covered by what the person just changed.
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
    /// The keyboard's input source changed: a layout, or a composing input method.
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

    /// Whether the readout is a level with a bar, rather than a state said in words.
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
    /// The glyph a sink called `name` plays through: headphones and headsets say so in their name,
    /// everything else is a speaker.
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

/// A reading with a bar: the volume, a display's brightness, the keyboard's backlight. `level` runs
/// `0.0 ..= 1.0` ; `on` is false when what it reads is muted or off, which fills nothing while the
/// bar keeps its place.
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

/// A mode change said in words ("Night light on"), with the chord that did it where there was one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    pub kind: OsdKind,
    #[serde(default = "on")]
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord: Option<String>,
    /// What the person calls what changed, where the kind alone does not say it: the name of the
    /// input source that now types.
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

/// How a readout's glyph is inked: lit, dimmed (the thing is off) or warning (the microphone is
/// muted and the person may be talking to it).
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

    /// The short value the pill reads: `65%` , `Muted` , `Off` .
    #[must_use]
    pub fn value_text(&self) -> String {
        let percent = || t_args("osd-percent", &[("percent", percent(self.level).into())]);
        match self.kind {
            OsdKind::Volume if !self.on => t("osd-muted"),
            OsdKind::KeyboardBacklight if self.level <= 0.0 => t("osd-off"),
            _ => percent(),
        }
    }

    /// What the pill calls the thing it read: the device's name, else the kind's.
    #[must_use]
    pub fn device_name(&self) -> String {
        self.device
            .as_ref()
            .map_or_else(|| kind_name(self.kind), |device| device.name.clone())
    }

    /// The glyph for the device it changed (its own when the session named it), and whether it
    /// reads as on.
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

    /// How much of the level bar is filled: a muted volume keeps its bar and empties its fill, so
    /// the pill does not change shape while still agreeing with the crossed-out volume glyph.
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
            // What the transport did, and the song it did it to: a pause names what stopped, which
            // the words alone cannot. The catalogue says both as one sentence, so the pill stays a
            // single line of text.
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

    /// The reading's `kind` .
    #[must_use]
    pub fn kind(&self) -> OsdKind {
        match self {
            Self::Level(level) => level.kind,
            Self::Hint(hint) => hint.kind,
        }
    }

    /// The action's flat arguments for this reading, on `display` (the connector of the display
    /// that changed), as any caller would pass them.
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

#[must_use]
pub fn osd_resource() -> ResourceSpec {
    offer::<OsdShow>(typed_resource::<Readout>(
        OSD_RESOURCE,
        t("desktop-actions-osd"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::i18n::ensure;

    fn volume(level: f32) -> Osd {
        Osd::from(Level {
            kind: OsdKind::Volume,
            level,
            on: true,
            device: Some(Device::new("Studio headphones", DeviceGlyph::Headphones)),
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

    tilo_test_env::scenario! {
        fn the_on_screen_display_reads_out_levels_and_hints;

        phase a_level_names_its_device_and_the_level_is_the_bar;
        {
            ensure();
            assert_eq!(
                volume(0.65).caption(),
                "Studio headphones, 65%",
                "a person who cannot see it hears the device and the level"
            );
            assert_eq!(
                volume(0.65).glyph(),
                (ui::icons::Icon::Headphones, GlyphTone::On)
            );
        }

        phase a_hint_says_the_mode_in_words_and_keeps_its_chord;
        {
            ensure();
            let hint = night_light(true);
            assert_eq!(hint.caption(), "Night light on");
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

        phase a_transport_hint_names_the_song_it_acted_on;
        {
            ensure();
            let named = |kind, song: Option<&str>| {
                Osd::from(Hint {
                    kind,
                    on: true,
                    chord: None,
                    name: song.map(str::to_owned),
                })
                .caption()
            };
            // A pause says what stopped, so the person knows which of the players in the room went
            // quiet; without a song it says what the key does.
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

        phase every_kind_has_a_glyph_and_says_something;
        {
            ensure();
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "spacing steps are whole logical pixels"
    )]
    fn the_readout_moves_out_of_the_text_carets_way() {
        let output = (0, 0, 1280, 800);
        assert_eq!(clear_of_caret(None, output), Placement::Bottom);
        // Typing in a field near the bottom centre: the readout stands at the top, clear of the
        // line being typed.
        let caret = Rect::new(620.0, 700.0, 628.0, 720.0);
        assert_eq!(clear_of_caret(Some(caret), output), Placement::Top);
        let (x, y, w, _) = place(output, Placement::Top);
        assert_eq!(x, 0);
        assert_eq!(y, (OSD_INSET - OSD_MARGIN) as i32);
        assert_eq!(w, 1280);
        // A caret far from the middle is no concern: the pill stays where a glance finds it.
        let far = Rect::new(40.0, 700.0, 48.0, 720.0);
        assert_eq!(clear_of_caret(Some(far), output), Placement::Bottom);
        // Nor is one in the middle of the screen, away from the foot.
        let middle = Rect::new(620.0, 300.0, 628.0, 320.0);
        assert_eq!(clear_of_caret(Some(middle), output), Placement::Bottom);
        // A second display is judged on its own foot.
        let second = (1280, 0, 1920, 1080);
        assert_eq!(clear_of_caret(Some(caret), second), Placement::Bottom);
    }

    /// The readout is published as one action whose effects carry the readout they declare: the
    /// device a key changed, and a mode as the words that say it.
    #[test]
    fn a_readout_action_carries_its_reading() {
        use tilo_resource::Effect as _;
        ensure();
        let resource = osd_resource();
        let spec = &resource.actions[0];
        assert_eq!(
            spec.action_type().contract.kind,
            tilo_actions::ActionKind::Undoable,
            "the readout's action is the undoable change it always was"
        );
        let show = |arguments: &serde_json::Map<String, serde_json::Value>| {
            tilo_actions::api::encode_json(
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
        let osd = OsdShow::from_payload(&encoded.payload)
            .expect("the action carries the readout it declares")
            .osd();
        assert_eq!(
            osd,
            Osd::from(Level {
                kind: OsdKind::Brightness,
                level: 0.3,
                on: true,
                device: Some(Device::new("Built-in display", DeviceGlyph::Display)),
            }),
            "the action carries the device the key changed"
        );
        // A mode change shows as a hint, with the chord that made it.
        let mut arguments = serde_json::Map::new();
        arguments.insert("kind".into(), "night-light".into());
        arguments.insert("chord".into(), "⌃⌥N".into());
        let encoded = show(&arguments);
        let osd = OsdShow::from_payload(&encoded.payload)
            .expect("the action carries the readout it declares")
            .osd();
        assert_eq!(osd, night_light(true));
        assert_eq!(osd.caption(), "Night light on");
    }
}
