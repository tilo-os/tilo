//! The session's displays, published: how they are used together — side by
//! side, showing the same, only the built-in or only the other ones — and how
//! bright each one is.
//!
//! The display key steps through the arrangements ([`DisplayMode::Next`]);
//! Settings, the display sheet and a script set one by name. Every display the
//! session shows is a row of the reading, with how bright it is when this
//! machine can set that, and the reading names the display the person works
//! on. The brightness keys, the Quick Settings sliders and the panel's readout
//! all read that one reading and invoke these one actions, so what a person
//! sees of their displays and what changes them are one thing.
//!
//! What the arrangements are carried out with is the compositor, which only
//! the session's loop reaches: those verbs queue a [`SessionCommand`] the loop
//! takes up on the same pass. Brightness is the machine's own hardware,
//! reached through [`Backlights`], so it is set here at once and the reading
//! says so; a display with neither a backlight nor DDC/CI is dimmed by the
//! compositor darkening what it shows, its ramps written on the loop
//! ([`Displays::dim`]).

use std::cell::RefCell;
use std::rc::Rc;

use os_actions::brightness::{BrightnessService, MonitorLevel, SessionDisplays};
use os_actions::{BrightnessKind, software_luminance, step_together};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_compositor::{Compositor, OutputChange, OutputInfo};

use tilo_desktop_surface::DesktopHandle;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::controls::SessionCommand;
use crate::i18n::t;

/// A way of using the displays together, or the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    /// The one after the current, in [`ARRANGEMENTS`] order.
    Next,
    /// Every display on, side by side: one desktop across them.
    Extend,
    /// Every display on, each showing the same.
    Mirror,
    /// Only the built-in display.
    BuiltIn,
    /// Only the other displays.
    External,
}

/// The arrangements, in the order the display key steps through them.
pub const ARRANGEMENTS: [DisplayMode; 4] = [
    DisplayMode::Extend,
    DisplayMode::Mirror,
    DisplayMode::BuiltIn,
    DisplayMode::External,
];

/// The names the `set-mode` action takes, in [`ARRANGEMENTS`] order.
pub const NAMES: [&str; 4] = ["extend", "mirror", "built-in", "external"];

impl DisplayMode {
    /// The arrangement named `name` ([`NAMES`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .zip(ARRANGEMENTS)
            .find_map(|(known, mode)| (*known == name).then_some(mode))
    }
}

/// Whether `name` is a built-in panel's connector.
fn built_in(name: &str) -> bool {
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The built-in display: the first panel connector, else the first output.
fn internal(outputs: &[OutputInfo]) -> Option<&str> {
    outputs
        .iter()
        .find(|output| built_in(&output.name))
        .or_else(|| outputs.first())
        .map(|output| output.name.as_str())
}

/// What a person calls `output` : the panel by its role, a monitor by the model its EDID names, and
/// a monitor that names none by its role too — its connector is how the machine knows it, not what
/// a person calls it.
fn display_name(output: &OutputInfo) -> String {
    if built_in(&output.name) {
        t("session-display-builtin")
    } else if output.label.is_empty() || output.label == output.name {
        t("session-display-external")
    } else {
        output.label.clone()
    }
}

/// A display the compositor's outputs put somewhere: its connector, whether it is on, where it
/// stands in logical `(x, y, w, h)` and the scale it draws at. What the shell's parts and the
/// panel's readout both read.
#[derive(Clone, Debug)]
pub(crate) struct Placed {
    /// The connector (`eDP-1`), which tells the displays apart.
    pub(crate) connector: String,
    /// Whether the display is on now.
    pub(crate) on: bool,
    /// Where it stands, logical `(x, y, w, h)` : signed, since a display left of or above the first
    /// starts below zero.
    pub(crate) rect: (i32, i32, i32, i32),
    /// The scale it draws at: what is painted on it is as crisp as the panel.
    pub(crate) scale: f64,
}

impl Placed {
    /// Where a surface host stands on it: its rectangle with sides at least one pixel, in the
    /// unsigned coordinates hosts lay a part out in.
    pub(crate) fn part_rect(&self) -> (i32, i32, u32, u32) {
        let (x, y, width, height) = self.rect;
        let side = |side: i32| u32::try_from(side.max(1)).unwrap_or(1);
        (x, y, side(width), side(height))
    }
}

/// Every display `outputs` put somewhere, in the order the compositor lists them: the ones with no
/// rectangle are shown nowhere.
#[must_use]
pub(crate) fn placed(outputs: &[OutputInfo]) -> Vec<Placed> {
    outputs
        .iter()
        .filter_map(|output| {
            Some(Placed {
                connector: output.name.clone(),
                on: output.enabled,
                rect: output.rect?,
                scale: output.scale,
            })
        })
        .collect()
}

/// The arrangement `outputs` show now.
#[must_use]
pub fn current(outputs: &[OutputInfo]) -> DisplayMode {
    let internal = internal(outputs);
    let on: Vec<&OutputInfo> = outputs.iter().filter(|output| output.enabled).collect();
    if on.len() < outputs.len() {
        return if on
            .iter()
            .any(|output| Some(output.name.as_str()) == internal)
        {
            DisplayMode::BuiltIn
        } else {
            DisplayMode::External
        };
    }
    let corner = |output: &&OutputInfo| output.rect.map(|(x, y, _, _)| (x, y));
    let first = on.first().and_then(corner);
    if on.len() > 1 && on.iter().all(|output| corner(output) == first) {
        DisplayMode::Mirror
    } else {
        DisplayMode::Extend
    }
}

/// The changes that arrange `outputs` as `mode` ; none with a single display, where there is
/// nothing to arrange.
#[must_use]
pub fn arrange(mode: DisplayMode, outputs: &[OutputInfo]) -> Vec<OutputChange> {
    if outputs.len() < 2 {
        return Vec::new();
    }
    let mode = match mode {
        DisplayMode::Next => {
            let now = current(outputs);
            ARRANGEMENTS
                .iter()
                .cycle()
                .skip_while(|mode| **mode != now)
                .nth(1)
                .copied()
                .unwrap_or(DisplayMode::Extend)
        }
        mode => mode,
    };
    let internal = internal(outputs).map(str::to_owned);
    let is_internal = |output: &OutputInfo| Some(&output.name) == internal.as_ref();
    match mode {
        DisplayMode::Next | DisplayMode::Extend => {
            // Left to right: the displays on now in the order they are known, then those off, whose
            // size is not known yet.
            let mut x = 0;
            let (lit, dark): (Vec<&OutputInfo>, Vec<&OutputInfo>) =
                outputs.iter().partition(|output| output.rect.is_some());
            lit.into_iter()
                .chain(dark)
                .map(|output| {
                    let change = OutputChange {
                        position: Some((x, 0)),
                        ..OutputChange::enable(output.name.clone())
                    };
                    if let Some((_, _, width, _)) = output.rect {
                        x = x.saturating_add(width);
                    }
                    change
                })
                .collect()
        }
        DisplayMode::Mirror => outputs
            .iter()
            .map(|output| OutputChange {
                position: Some((0, 0)),
                ..OutputChange::enable(output.name.clone())
            })
            .collect(),
        DisplayMode::BuiltIn | DisplayMode::External => {
            let keep = mode == DisplayMode::BuiltIn;
            outputs
                .iter()
                .map(|output| {
                    if is_internal(output) == keep {
                        OutputChange::enable(output.name.clone())
                    } else {
                        OutputChange::disable(output.name.clone())
                    }
                })
                .collect()
        }
    }
}

/// The resource the session publishes of its displays.
pub const RESOURCE: &str = "session:displays";

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = "session.displays";

/// How far a brightness key moves a display, as a share of full brightness.
pub const BRIGHTNESS_STEP: f32 = 0.1;

/// The displays as a view of another language reads and changes them: the reading ([`Reading`]) as
/// their fields and [`DisplaysAction`] as their verbs.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, DisplaysAction>("displays", "Displays")
}

/// What the displays resource offers: the arrangements, the display sheet,
/// and every display's brightness.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.displays")]
pub enum DisplaysAction {
    /// Use the displays together in the next arrangement
    #[effect(own_device, icon = "monitor")]
    NextMode,
    /// Use the displays together this way
    #[effect(own_device, icon = "monitor")]
    SetMode {
        /// One of [`NAMES`].
        #[param("How: extend, mirror, built-in or external", choices = NAMES)]
        mode: String,
    },
    /// Bring up the display sheet
    /// Where the layouts and the wireless displays are chosen.
    #[effect(own_device, icon = "monitor")]
    OpenSheet,
    /// Take the display sheet down
    #[effect(own_device, idempotent)]
    CloseSheet,
    /// Make the display brighter
    /// The display the person works on, or with `all` every display that can be dimmed, each
    /// keeping its share of the brightest one.
    #[effect(own_device, icon = "sun")]
    BrightnessUp {
        /// Whether every display moves together, as Shift does on the key.
        #[param("Whether every display moves together")]
        all: Option<bool>,
    },
    /// Make the display dimmer
    /// The display the person works on, or with `all` every display that can be dimmed, each
    /// keeping its share of the brightest one.
    #[effect(own_device, icon = "sun")]
    BrightnessDown { all: Option<bool> },
    /// Set the brightness
    /// The display the person works on, or the first one that can be dimmed.
    #[effect(own_device, idempotent, icon = "sun")]
    SetBrightness {
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Set one display's brightness
    #[effect(own_device, idempotent, icon = "sun")]
    SetDisplayBrightness {
        /// The display's connector, as the reading lists it.
        #[param("The display's connector")]
        display: String,
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
}

/// The machine's brightness controls, as this service reaches them: the OS brightness service in a
/// session on a machine, a stand-in in a test.
pub trait Backlights {
    /// Every display whose brightness this machine can set, built-in first,
    /// and how bright it is now (`0.0..=1.0`).
    fn levels(&self) -> Vec<MonitorLevel>;

    /// Set `connector` to `percent` (`0..=100`). Whether this machine has such a display to set.
    fn set(&self, connector: &str, percent: f32) -> bool;

    /// The displays the session shows: one it shows nothing on gets no brightness control.
    fn show_only(&self, connectors: &[String]);

    /// Every display this machine dims in software, by connector, and how dark it is on its slider
    /// (`0.0..=1.0`): the ones with neither a backlight nor DDC/CI, which the compositor darkens
    /// instead.
    fn software_levels(&self) -> Vec<(String, f32)>;

    /// Bumped whenever the software-dimmed levels or the displays they are of change; equal values
    /// mean [`Self::software_levels`] answers the same, so a pass that sees no change skips it.
    /// Controls that cannot tell answer `0` and are asked every pass.
    fn generation(&self) -> u64 {
        0
    }

    /// Wake `wake` when a level or the displays it controls change, for controls that report
    /// changes made elsewhere (a brightness key another program handled, a slider on another
    /// desktop): the software dimming follows on the next pass.
    fn watch(&self, _wake: &tilo_service::Wake) {}
}

/// The screens of this machine, as the OS brightness service reaches them: the panel's backlight
/// and every external monitor that speaks DDC/CI.
#[derive(Clone, Copy, Debug, Default)]
pub struct Screens;

impl Backlights for Screens {
    fn levels(&self) -> Vec<MonitorLevel> {
        BrightnessService::global().levels()
    }

    fn set(&self, connector: &str, percent: f32) -> bool {
        BrightnessService::global().set(connector, percent)
    }

    fn show_only(&self, connectors: &[String]) {
        BrightnessService::global().show_only(SessionDisplays {
            connectors: connectors.to_vec(),
            // A display with neither a backlight nor DDC/CI is dimmed by the compositor darkening
            // what it shows, so it keeps a control.
            software_dim: true,
        });
    }

    fn software_levels(&self) -> Vec<(String, f32)> {
        BrightnessService::global().software_levels()
    }

    fn generation(&self) -> u64 {
        BrightnessService::global().generation()
    }

    fn watch(&self, wake: &tilo_service::Wake) {
        let woken = std::sync::Arc::clone(wake);
        BrightnessService::global().on_change(std::sync::Arc::new(move || woken()));
    }
}

/// The displays as one reads them now: which ones the session shows, how bright each is, and which
/// one the person works on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Reading {
    /// Every display the session shows, the built-in one first.
    pub displays: Vec<Display>,
    /// The connector of the display the person works on, when the desk names one of the displays
    /// shown.
    pub focused: Option<String>,
    /// How bright the display a brightness key and `set-brightness` are about is
    /// ([`Reading::brightness`]), when one can be dimmed: what a slider over the displays shows.
    #[serde(default)]
    pub level: Option<f32>,
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

/// The session's displays, as the session holds them.
pub struct Displays {
    inner: Rc<RefCell<Inner>>,
}

impl Clone for Displays {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

/// What the session knows of its displays between passes.
#[derive(Default)]
struct Inner {
    /// The machine's brightness controls; `None` in a session that dims nothing (a test, a window
    /// inside another desktop).
    backlights: Option<Box<dyn Backlights>>,
    /// Where the arrangements land, for the loop to carry out.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
    /// The displays the compositor shows, as its outputs say now.
    shown: Vec<Shown>,
    /// The connector the person works on, when the desk names one.
    focused: Option<String>,
    /// The connectors the brightness controls were last told of.
    told: Vec<String>,
    /// The connector and the light each display the compositor dims in software was last told to
    /// show, so an unchanged level is not written again on every pass.
    dimmed: Vec<(String, f32)>,
    /// The software-dimmed generation `dimmed` was worked out from, when the controls can tell.
    dim_generation: Option<u64>,
}

/// One display the compositor shows.
struct Shown {
    connector: String,
    /// The monitor on it, as the desk follows it between ports.
    monitor: String,
    name: String,
    colour: DisplayCapabilities,
}

impl Inner {
    /// The reading as it stands now: every display shown, with how bright it is when this machine
    /// can set that.
    fn reading(&self) -> Reading {
        let levels = self.levels();
        let mut reading = Reading {
            displays: self
                .shown
                .iter()
                .map(|shown| {
                    let level = levels
                        .iter()
                        .find(|level| level.connector == shown.connector);
                    Display {
                        connector: shown.connector.clone(),
                        name: shown.name.clone(),
                        brightness: level.map(|level| level.level),
                        backlight: level
                            .is_some_and(|level| level.kind != BrightnessKind::Software),
                        colour: shown.colour,
                    }
                })
                .collect(),
            focused: self.focused.clone(),
            level: None,
        };
        reading.level = reading.brightness();
        reading
    }

    /// The displays that can be dimmed now: the machine's, of those the session shows.
    fn levels(&self) -> Vec<MonitorLevel> {
        let Some(backlights) = &self.backlights else {
            return Vec::new();
        };
        let mut levels = backlights.levels();
        levels.retain(|level| {
            self.shown
                .iter()
                .any(|shown| shown.connector == level.connector)
        });
        levels
    }
}

impl Displays {
    /// The displays over `backlights` , whose arrangements land as `commands` the loop carries out.
    #[must_use]
    pub fn new(
        backlights: Option<Box<dyn Backlights>>,
        commands: Rc<RefCell<Vec<SessionCommand>>>,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                backlights,
                commands,
                ..Inner::default()
            })),
        }
    }

    /// Mount it as one of the session's services, and hand back the handle the loop tells which
    /// displays it shows.
    pub fn mount(
        services: &Rc<RefCell<tilo_service::Services>>,
        backlights: Option<Box<dyn Backlights>>,
        commands: Rc<RefCell<Vec<SessionCommand>>>,
    ) -> Self {
        let displays = Self::new(backlights, commands);
        services.borrow_mut().add(Box::new(displays.clone()));
        displays
    }

    /// The compositor shows `outputs` now, and the person works on the display `handle` names: take
    /// them in, and tell the brightness controls which displays are shown, so a monitor nothing is
    /// shown on has no control.
    pub fn land(&self, handle: &DesktopHandle, outputs: &[OutputInfo]) {
        let shown: Vec<Shown> = outputs
            .iter()
            .filter(|output| output.enabled)
            .map(|output| Shown {
                connector: output.name.clone(),
                monitor: output.monitor.clone(),
                name: display_name(output),
                colour: output.colour,
            })
            .collect();
        let focused = handle
            .focused_display()
            .and_then(|display| {
                handle.read(|tree| tree.outputs.monitor_of(display).map(str::to_owned))
            })
            .and_then(|monitor| {
                shown
                    .iter()
                    .find(|shown| shown.monitor == monitor)
                    .map(|shown| shown.connector.clone())
            });
        let connectors: Vec<String> = shown.iter().map(|shown| shown.connector.clone()).collect();
        let mut inner = self.inner.borrow_mut();
        if inner.told != connectors {
            if let Some(backlights) = &inner.backlights {
                backlights.show_only(&connectors);
            }
            inner.told = connectors;
            // Which displays the dimming is worked out for moved.
            inner.dim_generation = None;
        }
        inner.shown = shown;
        inner.focused = focused;
    }

    /// Wake the session's loop whenever the brightness controls report a change, so a level set
    /// anywhere — the brightness keys, a slider, a script — reaches the software dimming on the
    /// next pass.
    pub fn watch(&self, wake: &tilo_service::Wake) {
        if let Some(backlights) = &self.inner.borrow().backlights {
            backlights.watch(wake);
        }
    }

    /// Darken the displays the brightness controls dim in software — the
    /// ones with neither a backlight nor DDC/CI — to the light their slider
    /// asks for, and leave every other display alone: its brightness is its
    /// own hardware. Nothing is written when the levels stand.
    ///
    /// # Errors
    /// The compositor could not write the ramps.
    pub fn dim(&self, compositor: &mut Compositor) -> Result<(), tilo_compositor::Error> {
        let (dims, generation, changed) = {
            let inner = self.inner.borrow();
            let Some(backlights) = &inner.backlights else {
                return Ok(());
            };
            let generation = backlights.generation();
            if inner.dim_generation == Some(generation) {
                return Ok(());
            }
            let mut dims: Vec<(String, f32)> = backlights
                .software_levels()
                .into_iter()
                .filter(|(connector, _)| {
                    inner
                        .shown
                        .iter()
                        .any(|shown| &shown.connector == connector)
                })
                .map(|(connector, level)| (connector, software_luminance(level)))
                .collect();
            dims.sort_by(|one, other| one.0.cmp(&other.0));
            let changed = inner.dimmed != dims;
            (dims, generation, changed)
        };
        if changed {
            compositor.set_software_dim(&dims)?;
        }
        let mut inner = self.inner.borrow_mut();
        inner.dimmed = dims;
        inner.dim_generation = Some(generation);
        Ok(())
    }

    /// The colour of the display on `monitor` : what a picture shown there is prepared for. A tile
    /// the desk gives no display of its own — one picture on every output — is shown on all of
    /// them, so it is only prepared for HDR while every one of them is driven with it, and no
    /// brighter than the dimmest.
    #[must_use]
    pub fn colour(&self, monitor: Option<&str>) -> DisplayCapabilities {
        let inner = self.inner.borrow();
        if let Some(monitor) = monitor {
            return inner
                .shown
                .iter()
                .find(|shown| shown.monitor == monitor)
                .map_or_else(DisplayCapabilities::sdr_default, |shown| shown.colour);
        }
        inner
            .shown
            .iter()
            .map(|shown| shown.colour)
            .reduce(shared_colour)
            .unwrap_or_else(DisplayCapabilities::sdr_default)
    }

    /// The display a level or step without one named is about: the one the person works on when it
    /// can be dimmed, else the first that can.
    fn target(&self) -> Result<String, DoorError> {
        let inner = self.inner.borrow();
        let levels = inner.levels();
        inner
            .focused
            .as_ref()
            .and_then(|focused| levels.iter().find(|level| &level.connector == focused))
            .or_else(|| levels.first())
            .map(|level| level.connector.clone())
            .ok_or_else(nothing_dimmable)
    }

    /// Move the display the person works on by `delta` , or with `all` every display that can be
    /// dimmed, each keeping its share of the brightest one's level.
    fn step(&self, delta: f32, all: bool) -> Result<(), DoorError> {
        let inner = self.inner.borrow();
        let levels = inner.levels();
        let Some(backlights) = &inner.backlights else {
            return Err(nothing_dimmable());
        };
        if levels.is_empty() {
            return Err(nothing_dimmable());
        }
        if all && levels.len() > 1 {
            let shares: Vec<f32> = levels.iter().map(|level| level.level).collect();
            for (level, next) in levels.iter().zip(step_together(&shares, delta)) {
                backlights.set(&level.connector, next * 100.0);
            }
            return Ok(());
        }
        let target = inner
            .focused
            .as_ref()
            .and_then(|focused| levels.iter().find(|level| &level.connector == focused))
            .or_else(|| levels.first())
            .ok_or_else(nothing_dimmable)?;
        let next = (target.level + delta).clamp(0.0, 1.0);
        if backlights.set(&target.connector, next * 100.0) {
            Ok(())
        } else {
            Err(nothing_dimmable())
        }
    }

    /// Set `connector` to `level` (`0.0..=1.0`).
    fn set(&self, connector: &str, level: f32) -> Result<(), DoorError> {
        let inner = self.inner.borrow();
        let Some(backlights) = &inner.backlights else {
            return Err(nothing_dimmable());
        };
        if backlights.set(connector, level.clamp(0.0, 1.0) * 100.0) {
            Ok(())
        } else {
            Err(DoorError::Unavailable {
                code: "unknown-display".into(),
                reason: format!("{connector} cannot be dimmed here"),
            })
        }
    }
}

/// The colour that suits a picture shown on a display showing `one` and on one showing `other` :
/// HDR only while both are driven with it, and no brighter than the dimmer of them.
fn shared_colour(one: DisplayCapabilities, other: DisplayCapabilities) -> DisplayCapabilities {
    if !(one.supports_pq_output && other.supports_pq_output) {
        return DisplayCapabilities::sdr_default();
    }
    DisplayCapabilities {
        max_luminance_nits: one.max_luminance_nits.min(other.max_luminance_nits),
        supports_pq_output: true,
        supports_hlg_output: one.supports_hlg_output && other.supports_hlg_output,
        bt2020_gamut: one.bt2020_gamut && other.bt2020_gamut,
    }
}

/// The refusal of a brightness key on a machine with nothing to dim.
fn nothing_dimmable() -> DoorError {
    DoorError::Unavailable {
        code: "no-backlight".into(),
        reason: "No display here can be dimmed".into(),
    }
}

/// `level` as a brightness, from 0 to 1.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a level from 0 to 1 fits an f32"
)]
fn level_of(level: f64) -> Result<f32, DoorError> {
    if level.is_finite() && (0.0..=1.0).contains(&level) {
        Ok(level as f32)
    } else {
        Err(DoorError::InvalidArguments(format!(
            "a brightness is from 0 to 1, not {level}"
        )))
    }
}

impl Service for Displays {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource =
            offer::<DisplaysAction>(ResourceSpec::new(RESOURCE, KIND, t("session-displays")));
        // The arrangements and the sheet keep the words the rest of the desktop names them by.
        for spec in &mut resource.actions {
            let label = match spec.id.as_str() {
                "next-mode" => t("session-displays-next"),
                "set-mode" => t("session-displays-set"),
                "open-sheet" => t("session-displays-open-sheet"),
                "close-sheet" => t("session-displays-close-sheet"),
                _ => continue,
            };
            spec.type_mut().label = label;
        }
        vec![resource]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.inner.borrow().reading()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<DisplaysAction>(&self.resources(), resource, action, arguments)? {
            DisplaysAction::NextMode => self.command(SessionCommand::Displays {
                mode: DisplayMode::Next,
            }),
            DisplaysAction::SetMode { mode } => {
                let mode = DisplayMode::named(&mode).ok_or_else(|| {
                    DoorError::InvalidArguments(format!("`{mode}` is not an arrangement"))
                })?;
                self.command(SessionCommand::Displays { mode });
            }
            DisplaysAction::OpenSheet => self.command(SessionCommand::OpenDisplaySheet),
            DisplaysAction::CloseSheet => self.command(SessionCommand::CloseDisplaySheet),
            DisplaysAction::BrightnessUp { all } => {
                self.step(BRIGHTNESS_STEP, all.unwrap_or(false))?;
            }
            DisplaysAction::BrightnessDown { all } => {
                self.step(-BRIGHTNESS_STEP, all.unwrap_or(false))?;
            }
            DisplaysAction::SetBrightness { level } => {
                let level = level_of(level)?;
                let display = self.target()?;
                self.set(&display, level)?;
            }
            DisplaysAction::SetDisplayBrightness { display, level } => {
                let level = level_of(level)?;
                self.set(&display, level)?;
            }
        }
        Ok(Started::Done(ActionOutput::default()))
    }
}

impl Displays {
    /// Queue `command` for the loop, which is the only one reaching the compositor.
    fn command(&self, command: SessionCommand) {
        self.inner.borrow().commands.borrow_mut().push(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(name: &str, x: i32) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            monitor: name.into(),
            label: name.into(),
            enabled: true,
            rect: Some((x, 0, 1280, 800)),
            scale: 1.0,
            description: None,
            colour: DisplayCapabilities::sdr_default(),
        }
    }

    #[test]
    fn the_display_key_steps_from_side_by_side_to_the_same_to_one_and_the_other() {
        let mut outputs = vec![on("HDMI-A-1", 1280), on("eDP-1", 0)];
        assert_eq!(current(&outputs), DisplayMode::Extend);
        let mirror = arrange(DisplayMode::Next, &outputs);
        assert!(mirror.iter().all(|change| change.position == Some((0, 0))));
        outputs = vec![on("HDMI-A-1", 0), on("eDP-1", 0)];
        assert_eq!(current(&outputs), DisplayMode::Mirror);
        let built_in = arrange(DisplayMode::Next, &outputs);
        assert_eq!(
            built_in
                .iter()
                .map(|change| (change.name.as_str(), change.enabled))
                .collect::<Vec<_>>(),
            [("HDMI-A-1", false), ("eDP-1", true)],
            "the panel is the built-in display wherever it is listed"
        );
        outputs[0].enabled = false;
        outputs[0].rect = None;
        assert_eq!(current(&outputs), DisplayMode::BuiltIn);
        assert!(!arrange(DisplayMode::Next, &outputs)[1].enabled);
        assert!(arrange(DisplayMode::Next, &outputs[1..]).is_empty());
        assert_eq!(DisplayMode::named("built-in"), Some(DisplayMode::BuiltIn));
    }

    /// The machine's brightness controls as a test lights them.
    #[derive(Clone, Default)]
    struct Dials(Rc<RefCell<DialsInner>>);

    #[derive(Default)]
    struct DialsInner {
        levels: Vec<MonitorLevel>,
        /// The connectors the service last said it shows.
        shown: Vec<String>,
        /// Every connector set, and to what, in order.
        set: Vec<(String, f32)>,
        /// Bumped on every change, as the OS brightness service does.
        generation: u64,
    }

    impl Dials {
        fn with(levels: &[(&str, f32)]) -> Self {
            let dials = Self::default();
            dials.0.borrow_mut().levels = levels
                .iter()
                .map(|(connector, level)| MonitorLevel {
                    connector: (*connector).into(),
                    level: *level,
                    kind: BrightnessKind::Backlight,
                })
                .collect();
            dials
        }

        /// The same, with `levels` dimmed in software instead of by their own hardware: the
        /// displays the compositor darkens.
        fn software(levels: &[(&str, f32)]) -> Self {
            let dials = Self::with(levels);
            for level in &mut dials.0.borrow_mut().levels {
                level.kind = BrightnessKind::Software;
            }
            dials
        }

        /// A level a display was set to by something other than this session (a key another program
        /// handled, a slider elsewhere).
        fn changed_elsewhere(&self, connector: &str, level: f32) {
            let mut inner = self.0.borrow_mut();
            if let Some(entry) = inner
                .levels
                .iter_mut()
                .find(|entry| entry.connector == connector)
            {
                entry.level = level;
            }
            inner.generation = inner.generation.saturating_add(1);
        }

        /// What each display is at now, by connector.
        fn at(&self, connector: &str) -> Option<f32> {
            self.0
                .borrow()
                .levels
                .iter()
                .find(|level| level.connector == connector)
                .map(|level| level.level)
        }
    }

    impl Backlights for Dials {
        fn levels(&self) -> Vec<MonitorLevel> {
            self.0.borrow().levels.clone()
        }

        fn set(&self, connector: &str, percent: f32) -> bool {
            let mut inner = self.0.borrow_mut();
            let Some(level) = inner
                .levels
                .iter_mut()
                .find(|level| level.connector == connector)
            else {
                return false;
            };
            level.level = percent / 100.0;
            inner.set.push((connector.to_owned(), percent));
            inner.generation = inner.generation.saturating_add(1);
            true
        }

        fn show_only(&self, connectors: &[String]) {
            self.0.borrow_mut().shown = connectors.to_vec();
        }

        fn software_levels(&self) -> Vec<(String, f32)> {
            self.0
                .borrow()
                .levels
                .iter()
                .filter(|entry| entry.kind == BrightnessKind::Software)
                .map(|entry| (entry.connector.clone(), entry.level))
                .collect()
        }

        fn generation(&self) -> u64 {
            self.0.borrow().generation
        }
    }

    /// A session showing `shown` (connector, name), over `backlights` .
    fn showing(backlights: Option<Dials>, shown: &[(&str, &str)]) -> Displays {
        let displays = Displays::new(
            backlights.map(|dials| Box::new(dials) as Box<dyn Backlights>),
            Rc::new(RefCell::new(Vec::new())),
        );
        let mut inner = displays.inner.borrow_mut();
        inner.shown = shown
            .iter()
            .map(|(connector, name)| Shown {
                connector: (*connector).into(),
                monitor: (*connector).into(),
                name: (*name).into(),
                colour: DisplayCapabilities::sdr_default(),
            })
            .collect();
        inner.told = shown
            .iter()
            .map(|(connector, _)| (*connector).to_owned())
            .collect();
        drop(inner);
        displays
    }

    /// The display on `connector` showing `colour` , as its sink and the signal it is sent would
    /// say.
    fn showing_colour(displays: &Displays, connector: &str, colour: DisplayCapabilities) {
        let mut inner = displays.inner.borrow_mut();
        if let Some(shown) = inner
            .shown
            .iter_mut()
            .find(|shown| shown.connector == connector)
        {
            shown.colour = colour;
        }
    }

    #[test]
    fn a_display_reports_the_colour_its_sink_and_signal_give_it() {
        let displays = showing(None, &[("eDP-1", "Built-in"), ("HDMI-A-1", "Dell")]);
        showing_colour(&displays, "HDMI-A-1", DisplayCapabilities::hdr10_baseline());
        assert_eq!(
            displays.colour(Some("eDP-1")),
            DisplayCapabilities::sdr_default()
        );
        assert_eq!(
            displays.colour(Some("HDMI-A-1")),
            DisplayCapabilities::hdr10_baseline(),
            "a display driven with HDR says so"
        );
        assert_eq!(
            displays.colour(Some("DP-9")),
            DisplayCapabilities::sdr_default(),
            "a monitor nothing is shown on is prepared for no more than SDR"
        );
        assert_eq!(
            reading(&displays).displays[1].colour,
            DisplayCapabilities::hdr10_baseline(),
            "the published reading carries it"
        );
    }

    #[test]
    fn a_display_is_named_by_its_model_never_by_its_connector() {
        let placed = |name: &str, label: &str| OutputInfo {
            name: name.to_owned(),
            monitor: name.to_owned(),
            label: label.to_owned(),
            enabled: true,
            rect: Some((0, 0, 1280, 800)),
            scale: 1.0,
            description: None,
            colour: DisplayCapabilities::sdr_default(),
        };
        assert_eq!(
            display_name(&placed("eDP-1", "eDP-1")),
            t("session-display-builtin"),
            "the built-in panel is the built-in panel"
        );
        assert_eq!(
            display_name(&placed("HDMI-A-1", "LG ULTRAGEAR+")),
            "LG ULTRAGEAR+",
            "a monitor is named by the model it reports"
        );
        assert_eq!(
            display_name(&placed("HDMI-A-1", "HDMI-A-1")),
            t("session-display-external"),
            "a monitor that names no model is not named by its connector"
        );
        assert_eq!(
            display_name(&placed("HDMI-A-1", "")),
            t("session-display-external"),
            "nor is one that names nothing"
        );
    }

    #[test]
    fn a_picture_on_every_display_is_prepared_for_what_they_all_show() {
        let displays = showing(None, &[("eDP-1", "Built-in"), ("HDMI-A-1", "Dell")]);
        assert_eq!(displays.colour(None), DisplayCapabilities::sdr_default());
        showing_colour(&displays, "HDMI-A-1", DisplayCapabilities::hdr10_baseline());
        assert_eq!(
            displays.colour(None),
            DisplayCapabilities::sdr_default(),
            "one display showing SDR takes HDR away from a picture on all of them"
        );
        showing_colour(
            &displays,
            "eDP-1",
            DisplayCapabilities {
                max_luminance_nits: 400.0,
                ..DisplayCapabilities::hdr10_baseline()
            },
        );
        let common = displays.colour(None);
        assert!(
            common.supports_pq_output,
            "every display showing HDR prepares the picture for HDR"
        );
        assert!(
            (common.max_luminance_nits - 400.0).abs() < f32::EPSILON,
            "no brighter than the dimmest display, got {}",
            common.max_luminance_nits
        );
    }

    /// What the resource publishes now.
    fn reading(displays: &Displays) -> Reading {
        let published = displays.read(RESOURCE).expect("the displays publish");
        serde_json::from_value(published).expect("the reading reads back")
    }

    /// `actual` is `expected` , within the precision a percentage is set at.
    #[track_caller]
    fn close(actual: Option<f32>, expected: f32) -> bool {
        actual.is_some_and(|actual| (actual - expected).abs() < 0.001)
    }

    fn invoke(
        displays: &mut Displays,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<(), DoorError> {
        displays.invoke(RESOURCE, action, arguments).map(drop)
    }

    #[test]
    fn the_reading_follows_a_brightness_change() {
        let dials = Dials::with(&[("eDP-1", 0.5), ("HDMI-A-1", 0.25), ("DP-9", 0.7)]);
        // The last display is one this session shows nothing on: no control.
        let displays = showing(
            Some(dials),
            &[("eDP-1", "Built-in display"), ("HDMI-A-1", "HDMI-A-1")],
        );
        let mut service = displays.clone();
        let before = reading(&displays);
        assert_eq!(
            before
                .displays
                .iter()
                .map(|display| (display.connector.as_str(), display.name.as_str()))
                .collect::<Vec<_>>(),
            [("eDP-1", "Built-in display"), ("HDMI-A-1", "HDMI-A-1")]
        );
        assert_eq!(before.displays[0].brightness, Some(0.5));
        assert!(before.displays[0].backlight);
        assert!(
            before
                .displays
                .iter()
                .all(|display| display.connector != "DP-9"),
            "a display nothing is shown on has no control"
        );

        // A display the session shows, by its connector: the slider's way.
        invoke(
            &mut service,
            "set-display-brightness",
            &Map::from_iter([
                ("display".to_owned(), Value::from("HDMI-A-1")),
                ("level".to_owned(), Value::from(0.9)),
            ]),
        )
        .expect("one display dims");
        let after = reading(&displays);
        assert!(close(after.displays[1].brightness, 0.9));
        assert_eq!(
            after.displays[0].brightness, before.displays[0].brightness,
            "the other display is untouched"
        );
    }

    #[test]
    fn shift_moves_every_dimmable_display_together() {
        let dials = Dials::with(&[("eDP-1", 0.5), ("HDMI-A-1", 0.25)]);
        let mut service = showing(
            Some(dials.clone()),
            &[("eDP-1", "Built-in"), ("HDMI-A-1", "Dell")],
        );
        invoke(
            &mut service,
            "brightness-up",
            &Map::from_iter([("all".to_owned(), Value::from(true))]),
        )
        .expect("every display moves");
        // The brightest steps 0.1 up; the dimmer keeps its share of it.
        assert!(close(dials.at("eDP-1"), 0.6));
        assert!(close(dials.at("HDMI-A-1"), 0.3));

        // Without `all` , only the display the person works on moves.
        let mut inner = service.inner.borrow_mut();
        inner.focused = Some("HDMI-A-1".into());
        drop(inner);
        invoke(&mut service, "brightness-down", &Map::new()).expect("the focused display dims");
        assert!(close(dials.at("HDMI-A-1"), 0.2));
        assert!(
            close(dials.at("eDP-1"), 0.6),
            "the display the person does not work on stays put"
        );
        assert_eq!(reading(&service).brightness(), dials.at("HDMI-A-1"));
    }

    /// A compositor showing `names` at 1280x800.
    fn compositor(names: &[&str]) -> Compositor {
        let mut compositor =
            Compositor::headless(Box::new(tilo_compositor::StackPolicy)).expect("compositor");
        for name in names {
            compositor
                .add_output(&tilo_compositor::OutputConfig::new(*name, 1280, 800))
                .expect("an output");
        }
        compositor
    }

    /// How much of its light `connector` shows, when the compositor dims it.
    fn dim_of(compositor: &Compositor, connector: &str) -> Option<f32> {
        compositor
            .software_dim()
            .into_iter()
            .find(|(name, _)| name == connector)
            .map(|(_, light)| light)
    }

    #[test]
    fn a_display_that_cannot_dim_its_own_light_is_darkened_by_the_compositor() {
        let dials = Dials::software(&[("eDP-1", 0.5), ("DP-9", 0.2)]);
        let displays = showing(
            Some(dials.clone()),
            &[("eDP-1", "Built-in"), ("HDMI-A-1", "Dell")],
        );
        let mut compositor = compositor(&["eDP-1", "HDMI-A-1"]);
        displays.dim(&mut compositor).expect("the dim lands");
        let light = dim_of(&compositor, "eDP-1").expect("the panel is dimmed in software");
        assert!(
            (light - software_luminance(0.5)).abs() < 0.001,
            "half on the slider dims to half its linear light, got {light}"
        );
        assert_eq!(
            dim_of(&compositor, "DP-9"),
            None,
            "a display the session shows nothing on is not dimmed"
        );
        assert_eq!(
            dim_of(&compositor, "HDMI-A-1"),
            None,
            "a display with a backlight of its own is not dimmed in software, and one at full is not written"
        );
        let shown = reading(&displays);
        assert!(close(shown.displays[0].brightness, 0.5));
        assert!(
            !shown.displays[0].backlight,
            "the reading says the compositor darkens it, not its own hardware"
        );

        // A level set elsewhere reaches the compositor, and leaving it alone writes nothing again.
        dials.changed_elsewhere("eDP-1", 0.25);
        displays.dim(&mut compositor).expect("the dim lands");
        let light = dim_of(&compositor, "eDP-1").expect("still dimmed");
        assert!(
            (light - software_luminance(0.25)).abs() < 0.001,
            "the new level darkens it further, got {light}"
        );
        // Back to full: no ramp is left lowering its light.
        dials.changed_elsewhere("eDP-1", 1.0);
        displays.dim(&mut compositor).expect("the dim lands");
        assert_eq!(dim_of(&compositor, "eDP-1"), None);
    }

    #[test]
    fn a_machine_that_dims_nothing_by_software_leaves_every_display_alone() {
        let displays = showing(
            Some(Dials::with(&[("eDP-1", 0.3)])),
            &[("eDP-1", "Built-in")],
        );
        let mut compositor = compositor(&["eDP-1"]);
        displays.dim(&mut compositor).expect("nothing to do");
        assert_eq!(
            dim_of(&compositor, "eDP-1"),
            None,
            "a backlight is dimmed where it is, not by the compositor"
        );
    }

    #[test]
    fn a_key_on_a_machine_that_dims_nothing_says_so() {
        let mut service = showing(None, &[("HDMI-A-1", "HDMI-A-1")]);
        let error = invoke(&mut service, "brightness-down", &Map::new()).expect_err("nothing dims");
        assert!(
            matches!(&error, DoorError::Unavailable { code, .. } if code == "no-backlight"),
            "{error:?}"
        );
        assert_eq!(reading(&service).brightness(), None);
        assert!(!reading(&service).displays[0].backlight);
    }

    #[test]
    fn a_level_outside_zero_to_one_is_refused() {
        let mut service = showing(
            Some(Dials::with(&[("eDP-1", 0.5)])),
            &[("eDP-1", "Built-in")],
        );
        for level in [-0.1, 1.5, f64::NAN] {
            let error = invoke(
                &mut service,
                "set-brightness",
                &Map::from_iter([("level".to_owned(), Value::from(level))]),
            )
            .expect_err("not a level");
            assert!(matches!(error, DoorError::InvalidArguments(_)), "{error:?}");
        }
    }
}
