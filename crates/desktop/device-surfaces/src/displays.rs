//! The displays as the device publishes them: the screens it shows, what a view reads of them,
//! and the modes and brightness it offers.

use serde::{Deserialize, Serialize};
use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_resource::Effect;

/// The names the `set-mode` action takes, in [`MODES`] order.
pub const NAMES: [&str; 4] = ["extend", "mirror", "built-in", "external"];

/// The display one of the shell's parts draws on, as that part reads it: its id, what a person
/// calls it, where it stands, the scale it draws at, and whether it is primary. The session
/// publishes it per part (`session:screen`), so a shell's view knows which display it is and
/// draws that display's strips and bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Screen {
    /// The display's id in this device's layout (`.id` is every resource's own id, so the
    /// display's is named for what it is).
    pub output: i64,
    /// The connector the compositor knows it by.
    pub connector: String,
    /// What a person calls it.
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    /// The scale it draws at.
    pub scale: f64,
    /// Whether it is the display the person made primary (the only one, on a device with one).
    pub primary: bool,
    /// Whether the person asked Settings which screen is which, so this display is saying so: its
    /// number and name stand on it for a moment.
    #[serde(default)]
    pub identify: bool,
    /// The number Settings gives it on its placement canvas, while it is saying so: `0` when it
    /// says nothing.
    #[serde(default)]
    pub identify_number: i64,
    /// What Settings calls it on that canvas, while it is saying so: empty when it says nothing.
    #[serde(default)]
    pub identify_name: String,
}

/// What the displays resource offers: the modes, the display sheet,
/// and every display's brightness.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.displays")]
pub enum DisplaysAction {
    /// Use the displays together in the next mode
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    NextMode,
    /// Use the displays together this way
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    SetMode {
        /// One of [`NAMES`].
        #[param("How: extend, mirror, built-in or external", choices = NAMES)]
        mode: String,
    },
    /// Bring up the display sheet
    /// Where the layouts and the wireless displays are chosen.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    OpenSheet,
    /// Take the display sheet down
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    CloseSheet,
    /// Make the display brighter
    /// The display the person works on, or with `all` every display that can be dimmed, each
    /// keeping its share of the brightest one.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "sun")]
    BrightnessUp {
        /// Whether every display moves together, as Shift does on the key.
        #[param("Whether every display moves together")]
        all: Option<bool>,
    },
    /// Make the display dimmer
    /// The display the person works on, or with `all` every display that can be dimmed, each
    /// keeping its share of the brightest one.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "sun")]
    BrightnessDown { all: Option<bool> },
    /// Set the brightness
    /// The display the person works on, or the first one that can be dimmed.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "sun")]
    SetBrightness {
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Set one display's brightness
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "sun")]
    SetDisplayBrightness {
        /// The display's connector, as the reading lists it.
        #[param("The display's connector")]
        display: String,
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
}

/// The displays as one reads them now: which ones the session shows, how bright each is, and which
/// one the person works on.
#[allow(
    clippy::struct_excessive_bools,
    reason = "what the screen says of each edge, read one at a time"
)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Reading {
    /// Every display the session shows, the built-in one first.
    pub displays: Vec<Display>,
    /// The connector of the display the person works on, when the shell names one of the displays
    /// shown.
    pub focused: Option<String>,
    /// How bright the display a brightness key and `set-brightness` are about is
    /// ([`Reading::brightness`]), when one can be dimmed: what a slider over the displays shows.
    #[serde(default)]
    pub level: Option<f32>,
    /// Whether the pointer is at, or resting against, the top edge: a bar that hides until it is
    /// reached for shows while this holds.
    #[serde(default)]
    pub bar_reached: bool,
    /// Whether a window stands where a top bar rides.
    #[serde(default)]
    pub bar_covered: bool,
    /// Whether the pointer is at the bottom edge: a dock there shows while this holds.
    #[serde(default)]
    pub dock_reached: bool,
    /// Whether a window stands where a dock rides.
    #[serde(default)]
    pub dock_covered: bool,
}

impl Reading {
    /// How bright the display the person works on is, when it can be dimmed,
    /// else the first display that can: the level a brightness key is about.
    #[must_use]
    pub fn brightness(&self) -> Option<f32> {
        self.focused
            .as_deref()
            .and_then(|focused| {
                self.displays
                    .iter()
                    .find(|display| display.connector == focused)
            })
            .and_then(|display| display.brightness)
            .or_else(|| self.displays.iter().find_map(|display| display.brightness))
    }
}

/// One display the session shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Display {
    /// Its DRM connector (`eDP-1`, `DP-2` ), which its brightness is set by.
    pub connector: String,
    /// What a person knows it by: the panel, its own name, else its connector.
    pub name: String,
    /// How bright it is, `0.0..=1.0` , when this machine can set it.
    pub brightness: Option<f32>,
    /// Whether its brightness is its own hardware — a backlight or DDC/CI — rather than the
    /// compositor darkening what it shows.
    pub backlight: bool,
    /// The colour it shows: what its sink accepts and the signal it is driven with, so what is
    /// shown on it is prepared for it.
    pub colour: DisplayCapabilities,
}

/// The displays as a view of another language reads and changes them: the reading ([`Reading`]) as
/// their fields and [`DisplaysAction`] as their verbs.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, DisplaysAction>("displays", "Displays")
}

/// The part's display, as a view reads it.
#[must_use]
pub fn screen_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Screen, tilo_resource::NoEffect>("screen", "Screen")
}
