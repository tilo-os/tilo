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
    /// Move a display on the arrangement
    /// Its top-left corner on the desktop, in logical pixels; the other displays stay where they
    /// are.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    ArrangeDisplays {
        /// The display's connector, as the reading lists it.
        #[param("The display's connector")]
        display: String,
        #[param("Left, in logical pixels")]
        x: i64,
        #[param("Top, in logical pixels")]
        y: i64,
    },
    /// Choose a display's resolution and refresh rate
    /// The change is tried for a moment and kept, or the previous mode comes back.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    SetDisplayMode {
        #[param("The display's connector")]
        display: String,
        #[param("Width in pixels")]
        width: i64,
        #[param("Height in pixels")]
        height: i64,
        /// In millihertz; left out, the fastest at that size.
        #[param("Refresh rate in millihertz")]
        refresh_mhz: Option<i64>,
    },
    /// Set a display's scale
    /// How many physical pixels it draws a logical pixel with.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "monitor")]
    SetDisplayScale {
        #[param("The display's connector")]
        display: String,
        #[param("Scale, such as 1, 1.25 or 2")]
        scale: f64,
    },
    /// Turn a display's picture
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "rotate-cw")]
    RotateDisplay {
        #[param("The display's connector")]
        display: String,
        /// One of [`TRANSFORMS`].
        #[param("Which way: normal, rotate-90, rotate-180 or rotate-270", choices = TRANSFORMS)]
        transform: String,
    },
    /// Make a display the primary one
    /// The display a shell tells apart from the others, and where it counts from.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "monitor")]
    MakeDisplayPrimary {
        #[param("The display's connector")]
        display: String,
    },
    /// Turn high dynamic range on or off
    /// The change is tried for a moment and kept, or the previous mode comes back.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor")]
    SetDisplayHdr {
        #[param("The display's connector")]
        display: String,
        #[param("Whether HDR is on")]
        on: bool,
    },
    /// Set how bright ordinary content looks in HDR
    /// In cd/m²; 203 is the reference white HDR video expects.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "sun")]
    SetDisplaySdrBrightness {
        #[param("The display's connector")]
        display: String,
        #[param("SDR brightness in cd/m², from 80 to 480")]
        nits: i64,
    },
    /// Turn a variable refresh rate on or off
    /// It engages on its own while a fullscreen window, like a game, is shown.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "monitor")]
    SetDisplayVrr {
        #[param("The display's connector")]
        display: String,
        #[param("Whether it may vary its refresh rate")]
        on: bool,
    },
    /// Choose a display's colour profile
    /// One of the profiles it offers, or none for the system default.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "palette")]
    SetDisplayColourProfile {
        #[param("The display's connector")]
        display: String,
        /// One of the display's profiles, as [`Display::colour_profiles`] names it; left out, the
        /// system default.
        #[param("The profile's key")]
        profile: Option<String>,
    },
    /// Number the screens
    /// Each display says its number and name for a moment, so a person sees which is which.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "monitor")]
    IdentifyDisplays,
    /// Keep the display change being tried
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "check")]
    KeepDisplayChange {
        /// The change's id, as the reading of the change names it.
        #[param("The change's id")]
        transaction: i64,
    },
    /// Put the displays back as they were
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "rotate-ccw")]
    RevertDisplayChange {
        /// The change's id, as the reading of the change names it.
        #[param("The change's id")]
        transaction: i64,
    },
    /// Let a display tool ask again
    /// Its risky display changes will wait to be kept, as any other program's do.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "shield-off")]
    ForgetDisplayTool {
        /// The executable, as [`TrustedTool::executable`] names it.
        #[param("The program's executable")]
        tool: String,
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
    /// How the displays are used together now: one of [`NAMES`].
    pub mode: String,
    /// The id of a display change being tried, `0` while none is. `keep-display-change` and
    /// `revert-display-change` answer for it.
    #[serde(default)]
    pub pending: i64,
    /// When the change being tried goes back unless it is kept, in Unix milliseconds.
    #[serde(default)]
    pub pending_until_unix_ms: Option<i64>,
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
    /// The display tools this device lets make risky changes without asking. A tool's changes
    /// apply at once; every other program's wait to be kept.
    #[serde(default)]
    pub trusted_tools: Vec<TrustedTool>,
}

/// A program this device trusts with its displays: one whose risky display changes apply
/// without asking.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct TrustedTool {
    /// The program's executable, which the trust is kept by: the compositor reads it from the
    /// connection's peer credentials.
    pub executable: String,
    /// What a person calls it: the executable's own name.
    pub name: String,
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one screen, read one at a time by a view"
)]
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
    /// Where its top-left corner stands in the desktop's logical layout. A display left of or
    /// above the first has a negative place.
    pub x: i64,
    pub y: i64,
    /// How much of the desktop it shows, in logical pixels.
    pub width: i64,
    pub height: i64,
    /// The scale it draws at: `/scale` follows it, so a panel is as crisp as it can be.
    pub scale: f64,
    /// Which way its picture is turned: `normal`, `rotate-90`, `rotate-180` or `rotate-270`.
    pub transform: String,
    /// Whether it is the display the person made primary.
    pub primary: bool,
    /// What it shows now, when its current mode is known.
    pub mode: Option<Mode>,
    /// Every mode it offers, as its panel and the signal to it allow.
    pub modes: Vec<Mode>,
    /// Whether it can show high dynamic range at all.
    pub hdr: bool,
    /// Whether it is showing high dynamic range now.
    #[serde(default)]
    pub hdr_on: bool,
    /// Whether its own panel asks for HDR, even when this machine cannot drive it.
    #[serde(default)]
    pub sink_hdr: bool,
    /// Why HDR cannot be driven here, when it cannot: `driver-unsupported`,
    /// `format-unsupported` or `driver-rejected`.
    #[serde(default)]
    pub hdr_unavailable: Option<String>,
    /// How bright ordinary content is while HDR is on, in cd/m².
    #[serde(default)]
    pub sdr_brightness_nits: i64,
    /// Whether it can vary its refresh rate.
    pub vrr: bool,
    /// Whether it may vary its refresh rate now.
    #[serde(default)]
    pub vrr_on: bool,
    /// Whether its own panel asks for a variable refresh rate, even when this machine cannot
    /// drive it.
    #[serde(default)]
    pub sink_vrr: bool,
    /// Why a variable refresh rate cannot be driven here, when it cannot: `driver-unsupported`
    /// or `driver-rejected`.
    #[serde(default)]
    pub vrr_unavailable: Option<String>,
    /// The colour profile it shows, when the person chose one.
    #[serde(default)]
    pub colour_profile: Option<String>,
    /// Every colour profile it offers.
    #[serde(default)]
    pub colour_profiles: Vec<ColourProfile>,
}

/// A colour profile a display offers: its stable key and the name a person reads.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct ColourProfile {
    /// The profile's stable key, which [`DisplaysAction::SetDisplayColourProfile`] takes.
    pub key: String,
    /// What a person reads: the profile's name, such as `sRGB`.
    pub label: String,
}

/// A size and rate a display can be driven at.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Mode {
    /// In pixels.
    pub width: i64,
    pub height: i64,
    /// In millihertz, so 59.94 Hz and 60 Hz stay distinct.
    pub refresh_mhz: i64,
}

impl Mode {
    /// How a person reads it: `1920 × 1080 at 60 Hz`.
    #[must_use]
    pub fn label(self) -> String {
        let hz = format!("{:.2}", self.refresh_mhz as f64 / 1000.0);
        let hz = hz.trim_end_matches('0').trim_end_matches('.');
        format!("{} × {} at {hz} Hz", self.width, self.height)
    }
}

/// The person's screens across their devices, as one layout: every screen each device shows,
/// where it stands in the shared space, and whether the pointer reaches it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PersonDisplays {
    /// This device, as the person's devices record names it.
    pub local: String,
    /// Every screen of theirs, this device's and the others'.
    pub screens: Vec<SharedScreen>,
}

/// One screen of the person's shared layout.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one screen, read one at a time by a view"
)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct SharedScreen {
    /// The device that drives it, by its key in the person's devices.
    pub device: String,
    /// The device's stable output id.
    pub output: String,
    /// What a person knows it by.
    pub name: String,
    /// What a person calls the device that drives it.
    pub device_label: String,
    /// Whether it is a screen of this device.
    pub local: bool,
    /// Whether the device that drives it is on a session with this one.
    pub online: bool,
    /// Where it stands in the shared space, in logical pixels.
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    /// Whether it meets a screen of another device along an edge, where the pointer crosses.
    pub reachable: bool,
}

/// Which way a display's picture is turned, as `rotate` takes it.
pub const TRANSFORMS: [&str; 4] = ["normal", "rotate-90", "rotate-180", "rotate-270"];

/// The displays as a view of another language reads and changes them: the reading ([`Reading`]) as
/// their fields and [`DisplaysAction`] as their verbs.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, DisplaysAction>("displays", "Displays")
}

/// The person's screens across their devices, as a view reads them: the reading ([`PersonDisplays`]),
/// with no verbs of its own. A screen's place is written through the display-placement section.
#[must_use]
pub fn person_displays_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<PersonDisplays, tilo_resource::NoEffect>(
        "person-displays",
        "Your screens",
    )
}

/// The part's display, as a view reads it.
#[must_use]
pub fn screen_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Screen, tilo_resource::NoEffect>("screen", "Screen")
}
