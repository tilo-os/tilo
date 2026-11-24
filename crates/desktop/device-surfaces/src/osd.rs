//! The hardware keys' readout as the device publishes it.

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// One readout: what it reads, whether it has a level or is said in words, and the display it
/// answers on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Readout {
    /// Whether the pill stands now; false once its hold ran out, so the shell draws nothing.
    pub visible: bool,
    /// What changed: `volume`, `brightness`, `keyboard-backlight`, `microphone`, `airplane-mode`,
    /// `touchpad`, `play`, `pause`, `next-track`, `previous-track`, `stop`, `no-backlight`,
    /// `night-light`, `do-not-disturb`, `magnifier`, `captured-keys`, `pointer-released` or
    /// `input-source`.
    pub kind: String,
    /// The level a level readout shows, from 0 to 1; none for a readout said in words.
    pub level: Option<f64>,
    /// Whether what it reads is on: false when the sound is muted or the level is off.
    pub on: bool,
    /// The device a level is about, the song a transport readout names, or the input source that
    /// now types. None when the kind alone says it.
    pub name: Option<String>,
    /// The glyph the level is about (`speaker`, `headphones`, `display`, `keyboard`).
    pub glyph: Option<String>,
    /// The display whose key changed it, by connector.
    pub display: Option<String>,
}

/// The readout as a view of another language reads it (`device.osd`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Readout, NoEffect>("osd", "Osd")
}
