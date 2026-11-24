//! The session's displays, published: how they are used together — side by
//! side, showing the same, only the built-in or only the other ones — and how
//! bright each one is.
//!
//! The display key steps through the modes ([`DisplayMode::Next`]);
//! Settings, the display sheet and a script set one by name. Every display the
//! session shows is a row of the reading, with how bright it is when this
//! machine can set that, and the reading names the display the person works
//! on. The brightness keys, the Quick Settings sliders and the panel's readout
//! all read that one reading and invoke these one actions, so what a person
//! sees of their displays and what changes them are one thing.
//!
//! What the modes are carried out with is the compositor, which only
//! the session's loop reaches: those verbs queue a [`SessionCommand`] the loop
//! takes up on the same pass. Brightness is the machine's own hardware,
//! reached through [`Backlights`], so it is set here at once and the reading
//! says so; a display with neither a backlight nor DDC/CI is dimmed by the
//! compositor darkening what it shows, its ramps written on the loop
//! ([`Displays::dim`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use os_actions::brightness::{BrightnessService, MonitorLevel, SessionDisplays};
use os_actions::{BrightnessKind, software_luminance, step_together};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_compositor::{Compositor, OutputChange, OutputInfo, Transform};

use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::controls::SessionCommand;
use crate::i18n::t;

/// A way of using the displays together, or the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    /// The one after the current, in [`MODES`] order.
    Next,
    /// Every display on, side by side: one desktop across them.
    #[default]
    Extend,
    /// Every display on, each showing the same.
    Mirror,
    /// Only the built-in display.
    BuiltIn,
    /// Only the other displays.
    External,
}

/// The modes, in the order the display key steps through them.
pub const MODES: [DisplayMode; 4] = [
    DisplayMode::Extend,
    DisplayMode::Mirror,
    DisplayMode::BuiltIn,
    DisplayMode::External,
];

pub use tilo_device_surfaces::displays::{
    ColourProfile, Display, DisplaysAction, Mode, NAMES, Reading, Screen, TRANSFORMS,
};

/// How bright ordinary content is in HDR unless the person chose another: the ITU BT.2408
/// reference white.
const DEFAULT_SDR_BRIGHTNESS_NITS: i64 = 203;

/// The range of SDR content brightness, in cd/m², that a display takes.
const SDR_BRIGHTNESS_NITS: std::ops::RangeInclusive<i64> = 80..=480;

impl DisplayMode {
    /// The mode named `name` ([`NAMES`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .zip(MODES)
            .find_map(|(known, mode)| (*known == name).then_some(mode))
    }

    /// The name a view reads it by ([`NAMES`]).
    #[must_use]
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .zip(MODES)
            .find_map(|(known, mode)| (mode == self).then_some(*known))
            .unwrap_or(NAMES[0])
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
pub(crate) fn display_name(output: &OutputInfo) -> String {
    if built_in(&output.name) {
        t("session-display-builtin")
    } else if output.label.is_empty() || output.label == output.name {
        t("session-display-external")
    } else {
        output.label.clone()
    }
}

/// Which way a display's picture is turned, as the reading names it.
fn transform_name(transform: Transform) -> &'static str {
    match transform {
        Transform::Normal => "normal",
        Transform::_90 => "rotate-90",
        Transform::_180 => "rotate-180",
        Transform::_270 => "rotate-270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped-90",
        Transform::Flipped180 => "flipped-180",
        Transform::Flipped270 => "flipped-270",
    }
}

/// The turn `name` (one of [`TRANSFORMS`]) stands for, else `None`.
fn transform_of(name: &str) -> Option<settings_store::hardware::OutputTransform> {
    Some(match name {
        "normal" => settings_store::hardware::OutputTransform::Normal,
        "rotate-90" => settings_store::hardware::OutputTransform::Rotate90,
        "rotate-180" => settings_store::hardware::OutputTransform::Rotate180,
        "rotate-270" => settings_store::hardware::OutputTransform::Rotate270,
        _ => return None,
    })
}

/// Why HDR cannot be driven on a display, as the reading names it.
fn hdr_reason(reason: settings_store::hardware::HdrUnavailable) -> String {
    use settings_store::hardware::HdrUnavailable;
    match reason {
        HdrUnavailable::DriverUnsupported => "driver-unsupported",
        HdrUnavailable::FormatUnsupported => "format-unsupported",
        HdrUnavailable::DriverRejected => "driver-rejected",
    }
    .to_owned()
}

/// Why a variable refresh rate cannot be driven on a display, as the reading names it.
fn vrr_reason(reason: settings_store::hardware::AdaptiveSyncUnavailable) -> String {
    use settings_store::hardware::AdaptiveSyncUnavailable;
    match reason {
        AdaptiveSyncUnavailable::DriverUnsupported => "driver-unsupported",
        AdaptiveSyncUnavailable::DriverRejected => "driver-rejected",
    }
    .to_owned()
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

/// The mode `outputs` show now.
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
            MODES
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
pub use tilo_session_core::shell::DISPLAYS_RESOURCE as RESOURCE;

/// The displays as the session publishes them: its declared type, what a brightness key and the
/// displays sheet change.
#[derive(tilo_resource::Row)]
#[row(kind = "session.displays", label = "Displays")]
pub struct DisplaysRoot;

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = <DisplaysRoot as tilo_resource::words::RowType>::KIND;

/// How far a brightness key moves a display, as a share of full brightness.
pub const BRIGHTNESS_STEP: f32 = 0.1;

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
    /// Where the modes land, for the loop to carry out.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
    /// The displays the compositor shows, as its outputs say now.
    shown: Vec<Shown>,
    /// The connector the person works on, when the shell names one.
    focused: Option<String>,
    /// The connectors the brightness controls were last told of.
    told: Vec<String>,
    /// The connector and the light each display the compositor dims in software was last told to
    /// show, so an unchanged level is not written again on every pass.
    dimmed: Vec<(String, f32)>,
    /// The software-dimmed generation `dimmed` was worked out from, when the controls can tell.
    dim_generation: Option<u64>,
    /// What the screen says of its edges, and whether that moved since the last pass.
    edges: tilo_session_core::shell::ScreenEdges,
    edges_moved: bool,
    /// Whether what the person's settings say of the displays changed since the last pass: a
    /// change made anywhere, not only through this service's own actions.
    settings_moved: bool,
    /// How the outputs put the displays together on the last pass.
    arrangement: DisplayMode,
    /// The change being tried, as its id and when it goes back, and the settings revision it was
    /// read at.
    pending: Option<(u64, Option<u64>)>,
    pending_revision: u64,
    /// The configuration the displays run with, as the person's settings keep it: each one's HDR,
    /// variable refresh rate, SDR brightness and colour profile.
    config: settings_store::hardware::DisplayConfig,
    /// The person's display placement, as their settings keep it.
    placements: settings_store::person_displays::Placements,
    /// The settings revision and monitor set `config` and `placements` were read at, so an
    /// unchanged document is not read again.
    followed: Option<(Option<settings_store::SettingsStamp>, String)>,
    /// Where this person's display settings are kept, which a change tried from a view is
    /// written to; `None` in a session with no settings document.
    source: Option<Arc<dyn settings_store::SettingsSource>>,
}

/// One display the compositor shows.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one screen, read one at a time by a view"
)]
struct Shown {
    connector: String,
    /// The monitor on it, as the shell follows it between ports.
    monitor: String,
    name: String,
    colour: DisplayCapabilities,
    /// Where it stands, logical `(x, y, w, h)`.
    rect: (i32, i32, i32, i32),
    scale: f64,
    transform: String,
    primary: bool,
    mode: Option<Mode>,
    modes: Vec<Mode>,
    hdr: bool,
    hdr_on: bool,
    sink_hdr: bool,
    hdr_unavailable: Option<String>,
    sdr_brightness_nits: i64,
    vrr: bool,
    vrr_on: bool,
    sink_vrr: bool,
    vrr_unavailable: Option<String>,
    colour_profile: Option<String>,
    colour_profiles: Vec<ColourProfile>,
}

impl Inner {
    /// Read the person's display settings and placement again when the settings document or the
    /// monitors they are kept for changed: a display's HDR, variable refresh rate, SDR brightness
    /// and colour profile come from there.
    fn follow_settings(
        &mut self,
        snapshot: Option<&settings_store::hardware::DisplayHardwareSnapshot>,
    ) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let descriptors: Vec<settings_store::hardware::OutputDescriptor> = snapshot
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        let set = settings_store::hardware::DisplaySet::of_descriptors(&descriptors);
        let followed = (settings_store::stamp(), set.key.clone());
        if self.followed.as_ref() == Some(&followed) {
            return;
        }
        let config = settings_store::hardware::DisplayConfig::load_effective_remembering(&set).0;
        self.settings_moved |= self.config != config;
        self.config = config;
        self.placements = settings_store::person_displays::Placements::load();
        self.followed = Some(followed);
    }

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
                        x: i64::from(shown.rect.0),
                        y: i64::from(shown.rect.1),
                        width: i64::from(shown.rect.2),
                        height: i64::from(shown.rect.3),
                        scale: shown.scale,
                        transform: shown.transform.clone(),
                        primary: shown.primary,
                        mode: shown.mode,
                        modes: shown.modes.clone(),
                        hdr: shown.hdr,
                        hdr_on: shown.hdr_on,
                        sink_hdr: shown.sink_hdr,
                        hdr_unavailable: shown.hdr_unavailable.clone(),
                        sdr_brightness_nits: shown.sdr_brightness_nits,
                        vrr: shown.vrr,
                        vrr_on: shown.vrr_on,
                        sink_vrr: shown.sink_vrr,
                        vrr_unavailable: shown.vrr_unavailable.clone(),
                        colour_profile: shown.colour_profile.clone(),
                        colour_profiles: shown.colour_profiles.clone(),
                    }
                })
                .collect(),
            mode: self.arrangement.name().to_owned(),
            pending: self
                .pending
                .map_or(0, |(id, _)| i64::try_from(id).unwrap_or(0)),
            pending_until_unix_ms: self
                .pending
                .and_then(|(_, deadline)| deadline)
                .and_then(|deadline| i64::try_from(deadline).ok()),
            focused: self.focused.clone(),
            level: None,
            bar_reached: self.edges.bar_reached,
            bar_covered: self.edges.bar_covered,
            dock_reached: self.edges.dock_reached,
            dock_covered: self.edges.dock_covered,
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
    /// The displays over `backlights` , whose modes land as `commands` the loop carries out, as
    /// `source` keeps the person's settings.
    #[must_use]
    pub fn new(
        backlights: Option<Box<dyn Backlights>>,
        commands: Rc<RefCell<Vec<SessionCommand>>>,
        source: Option<Arc<dyn settings_store::SettingsSource>>,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                backlights,
                commands,
                source,
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
        source: Option<Arc<dyn settings_store::SettingsSource>>,
    ) -> Self {
        let displays = Self::new(backlights, commands, source);
        services.borrow_mut().add(Box::new(displays.clone()));
        displays
    }

    /// The compositor shows `outputs` now, the person works on the one named `focused` (its
    /// connector), and `snapshot` is what it says of each one's modes: take them in, and tell the
    /// brightness controls which displays are shown, so a monitor nothing is shown on has no
    /// control.
    pub fn land(
        &self,
        focused: Option<String>,
        outputs: &[OutputInfo],
        snapshot: Option<&settings_store::hardware::DisplayHardwareSnapshot>,
    ) {
        let mut inner = self.inner.borrow_mut();
        inner.follow_settings(snapshot);
        let shown: Vec<Shown> = outputs
            .iter()
            .filter(|output| output.enabled)
            .map(|output| {
                let observed = snapshot.and_then(|snapshot| {
                    snapshot
                        .outputs
                        .iter()
                        .find(|observed| observed.descriptor.connector == output.name)
                });
                let config = inner.config.outputs.get(&output.name);
                Shown {
                    connector: output.name.clone(),
                    monitor: output.monitor.clone(),
                    name: display_name(output),
                    colour: output.colour,
                    rect: output.rect.unwrap_or_default(),
                    scale: output.scale,
                    transform: transform_name(output.transform).to_owned(),
                    primary: output.primary,
                    mode: observed
                        .map(|observed| Mode {
                            width: i64::from(observed.current_mode.width),
                            height: i64::from(observed.current_mode.height),
                            refresh_mhz: i64::from(observed.current_mode.refresh_mhz),
                        })
                        .or_else(|| {
                            output.mode.map(|(width, height, refresh_mhz)| Mode {
                                width: i64::from(width),
                                height: i64::from(height),
                                refresh_mhz: i64::from(refresh_mhz),
                            })
                        }),
                    modes: observed.map_or_else(Vec::new, |observed| {
                        observed
                            .descriptor
                            .modes
                            .iter()
                            .map(|mode| Mode {
                                width: i64::from(mode.width),
                                height: i64::from(mode.height),
                                refresh_mhz: i64::from(mode.refresh_mhz),
                            })
                            .collect()
                    }),
                    hdr: observed.is_some_and(|observed| observed.descriptor.hdr),
                    hdr_on: config.map_or(output.colour.supports_pq_output, |config| config.hdr),
                    sink_hdr: observed.is_some_and(|observed| observed.sink_hdr),
                    hdr_unavailable: observed
                        .and_then(|observed| observed.hdr_unavailable_reason)
                        .map(hdr_reason),
                    sdr_brightness_nits: config.map_or(DEFAULT_SDR_BRIGHTNESS_NITS, |config| {
                        i64::from(config.sdr_brightness_nits)
                    }),
                    vrr: observed.is_some_and(|observed| observed.descriptor.vrr),
                    vrr_on: config.is_none_or(|config| config.vrr),
                    sink_vrr: observed.is_some_and(|observed| observed.sink_vrr),
                    vrr_unavailable: observed
                        .and_then(|observed| observed.vrr_unavailable_reason)
                        .map(vrr_reason),
                    colour_profile: config.and_then(|config| config.color_profile.clone()),
                    colour_profiles: observed.map_or_else(Vec::new, |observed| {
                        observed
                            .descriptor
                            .color_profiles
                            .iter()
                            .filter_map(|id| {
                                observed
                                    .color_profile_labels
                                    .get(id)
                                    .map(|label| ColourProfile {
                                        key: id.clone(),
                                        label: label.clone(),
                                    })
                            })
                            .collect()
                    }),
                }
            })
            .collect();
        let arrangement = current(outputs);
        let connectors: Vec<String> = shown.iter().map(|shown| shown.connector.clone()).collect();
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
        inner.arrangement = arrangement;
        if let Some(source) = &inner.source {
            let revision = settings_store::SettingsSource::revision(source.as_ref());
            if inner.pending_revision != revision {
                inner.pending = settings_store::hardware::DisplayTransaction::load()
                    .filter(|transaction| {
                        matches!(
                            transaction.state,
                            settings_store::hardware::DisplayTransactionState::Requested
                                | settings_store::hardware::DisplayTransactionState::AwaitingConfirmation
                                | settings_store::hardware::DisplayTransactionState::ConfirmRequested
                        )
                    })
                    .map(|transaction| (transaction.id, transaction.deadline_unix_ms));
                inner.pending_revision = revision;
            }
        }
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
    /// the shell gives no display of its own — one picture on every output — is shown on all of
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
    fn target(&self) -> Result<String, ApiError> {
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
    fn step(&self, delta: f32, all: bool) -> Result<(), ApiError> {
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
    fn set(&self, connector: &str, level: f32) -> Result<(), ApiError> {
        let inner = self.inner.borrow();
        let Some(backlights) = &inner.backlights else {
            return Err(nothing_dimmable());
        };
        if backlights.set(connector, level.clamp(0.0, 1.0) * 100.0) {
            Ok(())
        } else {
            Err(ApiError::Unavailable {
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
fn nothing_dimmable() -> ApiError {
    ApiError::Unavailable {
        code: "no-backlight".into(),
        reason: "No display here can be dimmed".into(),
    }
}

/// `level` as a brightness, from 0 to 1.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a level from 0 to 1 fits an f32"
)]
fn level_of(level: f64) -> Result<f32, ApiError> {
    if level.is_finite() && (0.0..=1.0).contains(&level) {
        Ok(level as f32)
    } else {
        Err(ApiError::InvalidArguments(format!(
            "a brightness is from 0 to 1, not {level}"
        )))
    }
}

impl Displays {
    /// The displays the compositor shows now, the monitors they are arranged for, and the
    /// configuration they run with: the starting point of a change a view asks for.
    fn running(
        &self,
    ) -> Result<
        (
            settings_store::hardware::DisplayHardwareSnapshot,
            settings_store::hardware::DisplaySet,
            settings_store::hardware::DisplayConfig,
        ),
        ApiError,
    > {
        let observed =
            settings_store::hardware::DisplayHardwareSnapshot::load().ok_or_else(|| {
                ApiError::Unavailable {
                    code: "displays-unknown".into(),
                    reason: "The connected displays are not known yet.".into(),
                }
            })?;
        let descriptors: Vec<settings_store::hardware::OutputDescriptor> = observed
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        if descriptors.is_empty() {
            return Err(ApiError::Unavailable {
                code: "no-displays".into(),
                reason: "No display is connected.".into(),
            });
        }
        let set = settings_store::hardware::DisplaySet::of_descriptors(&descriptors);
        let config = settings_store::display_control::current(&observed);
        Ok((observed, set, config))
    }

    /// Where this person's display settings are written.
    fn settings_source(&self) -> Result<Arc<dyn settings_store::SettingsSource>, ApiError> {
        self.inner
            .borrow()
            .source
            .clone()
            .ok_or_else(|| ApiError::Unavailable {
                code: "no-settings".into(),
                reason: "This session keeps no display settings.".into(),
            })
    }

    /// Try the change `edit` makes: the desktop applies it, and the person keeps it or the
    /// displays go back as they were.
    fn stage(
        &self,
        edit: impl FnOnce(
            &settings_store::hardware::DisplayHardwareSnapshot,
            &mut settings_store::hardware::DisplayConfig,
        ) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        let (observed, set, mut candidate) = self.running()?;
        edit(&observed, &mut candidate)?;
        let source = self.settings_source()?;
        settings_store::display_control::execute(
            source.as_ref(),
            observed,
            settings_store::display_control::DisplayCommand::Apply {
                observed_set: set,
                candidate,
            },
        )
        .map(drop)
        .map_err(ApiError::Failed)
    }

    /// Say which screen is which, on the screens themselves.
    fn identify(&self) -> Result<(), ApiError> {
        let (observed, _, _) = self.running()?;
        let source = self.settings_source()?;
        settings_store::display_control::execute(
            source.as_ref(),
            observed,
            settings_store::display_control::DisplayCommand::Identify,
        )
        .map(drop)
        .map_err(ApiError::Failed)
    }

    /// Answer for the change `transaction` being tried: keep it, or put the previous display
    /// configuration back.
    fn decide(&self, transaction: i64, keep: bool) -> Result<(), ApiError> {
        let transaction = u64::try_from(transaction)
            .map_err(|_| ApiError::InvalidArguments("the change's id is not a number".into()))?;
        let (observed, _, _) = self.running()?;
        let source = self.settings_source()?;
        let command = if keep {
            settings_store::display_control::DisplayCommand::Keep { transaction }
        } else {
            settings_store::display_control::DisplayCommand::Revert { transaction }
        };
        settings_store::display_control::execute(source.as_ref(), observed, command)
            .map(drop)
            .map_err(ApiError::Failed)
    }

    /// What the screen says of its edges now: the pointer at an edge, and a window where
    /// a bar or a dock there rides. What a bar or dock that shows only sometimes follows.
    /// Whether they moved.
    pub(crate) fn set_edges(&self, edges: tilo_session_core::shell::ScreenEdges) -> bool {
        let mut inner = self.inner.borrow_mut();
        if inner.edges == edges {
            return false;
        }
        inner.edges = edges;
        inner.edges_moved = true;
        true
    }
}

impl Service for Displays {
    fn poll(&mut self) -> bool {
        let mut inner = self.inner.borrow_mut();
        let edges = std::mem::take(&mut inner.edges_moved);
        let settings = std::mem::take(&mut inner.settings_moved);
        edges || settings
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource = offer::<DisplaysAction>(
            tilo_resource::LiveRow::typed::<DisplaysRoot>(RESOURCE, t("session-displays"))
                .into_resource(),
        );
        // The modes and the sheet keep the words the rest of the desktop names them by.
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
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<DisplaysAction>(&self.resources(), resource, action, arguments)? {
            DisplaysAction::NextMode => self.command(SessionCommand::Displays {
                mode: DisplayMode::Next,
            }),
            DisplaysAction::SetMode { mode } => {
                let mode = DisplayMode::named(&mode).ok_or_else(|| {
                    ApiError::InvalidArguments(format!("`{mode}` is not a display mode"))
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
            DisplaysAction::ArrangeDisplays { display, x, y } => {
                self.stage(|_, config| {
                    let output = config.outputs.get_mut(&display).ok_or_else(|| {
                        ApiError::InvalidArguments(format!("`{display}` is not a display"))
                    })?;
                    output.x = i32::try_from(x).map_err(|_| {
                        ApiError::InvalidArguments("the place is out of range".into())
                    })?;
                    output.y = i32::try_from(y).map_err(|_| {
                        ApiError::InvalidArguments("the place is out of range".into())
                    })?;
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplayMode {
                display,
                width,
                height,
                refresh_mhz,
            } => {
                self.stage(|observed, config| {
                    let offered = observed
                        .outputs
                        .iter()
                        .find(|output| output.descriptor.connector == display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?;
                    let matches = |mode: &&settings_store::hardware::OutputMode| {
                        i64::from(mode.width) == width
                            && i64::from(mode.height) == height
                            && refresh_mhz
                                .is_none_or(|refresh| i64::from(mode.refresh_mhz) == refresh)
                    };
                    let mode = offered
                        .descriptor
                        .modes
                        .iter()
                        .filter(matches)
                        .max_by_key(|mode| mode.refresh_mhz)
                        .copied()
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!(
                                "{width} × {height} is not a mode of `{display}`"
                            ))
                        })?;
                    let output = config.outputs.get_mut(&display).ok_or_else(|| {
                        ApiError::InvalidArguments(format!("`{display}` is not a display"))
                    })?;
                    output.mode = Some(mode);
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplayScale { display, scale } => {
                self.stage(|_, config| {
                    let output = config.outputs.get_mut(&display).ok_or_else(|| {
                        ApiError::InvalidArguments(format!("`{display}` is not a display"))
                    })?;
                    if !(scale.is_finite() && scale > 0.0) {
                        return Err(ApiError::InvalidArguments(
                            "the scale is not a positive number".into(),
                        ));
                    }
                    output.scale = scale;
                    Ok(())
                })?;
            }
            DisplaysAction::RotateDisplay { display, transform } => {
                let transform = transform_of(&transform).ok_or_else(|| {
                    ApiError::InvalidArguments(format!("`{transform}` is not a turn"))
                })?;
                self.stage(|_, config| {
                    let output = config.outputs.get_mut(&display).ok_or_else(|| {
                        ApiError::InvalidArguments(format!("`{display}` is not a display"))
                    })?;
                    output.transform = transform;
                    Ok(())
                })?;
            }
            DisplaysAction::MakeDisplayPrimary { display } => {
                self.stage(|_, config| {
                    if !config.outputs.contains_key(&display) {
                        return Err(ApiError::InvalidArguments(format!(
                            "`{display}` is not a display"
                        )));
                    }
                    for (connector, output) in &mut config.outputs {
                        output.primary = *connector == display;
                    }
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplayHdr { display, on } => {
                self.stage(|observed, config| {
                    let offered = observed
                        .outputs
                        .iter()
                        .find(|output| output.descriptor.connector == display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?;
                    if on && !offered.descriptor.hdr {
                        return Err(ApiError::Unavailable {
                            code: "no-hdr".into(),
                            reason: format!("`{display}` cannot show high dynamic range"),
                        });
                    }
                    config
                        .outputs
                        .get_mut(&display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?
                        .hdr = on;
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplaySdrBrightness { display, nits } => {
                if !SDR_BRIGHTNESS_NITS.contains(&nits) {
                    return Err(ApiError::InvalidArguments(format!(
                        "SDR brightness is from {start} to {end} cd/m², not {nits}",
                        start = SDR_BRIGHTNESS_NITS.start(),
                        end = SDR_BRIGHTNESS_NITS.end(),
                    )));
                }
                let nits = u16::try_from(nits).map_err(|_| {
                    ApiError::InvalidArguments("the SDR brightness is out of range".into())
                })?;
                self.stage(|_, config| {
                    config
                        .outputs
                        .get_mut(&display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?
                        .sdr_brightness_nits = nits;
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplayVrr { display, on } => {
                self.stage(|_, config| {
                    config
                        .outputs
                        .get_mut(&display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?
                        .vrr = on;
                    Ok(())
                })?;
            }
            DisplaysAction::SetDisplayColourProfile { display, profile } => {
                self.stage(|observed, config| {
                    let offered = observed
                        .outputs
                        .iter()
                        .find(|output| output.descriptor.connector == display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?;
                    if let Some(profile) = &profile
                        && !offered.descriptor.color_profiles.contains(profile)
                    {
                        return Err(ApiError::InvalidArguments(format!(
                            "`{profile}` is not a colour profile of `{display}`"
                        )));
                    }
                    config
                        .outputs
                        .get_mut(&display)
                        .ok_or_else(|| {
                            ApiError::InvalidArguments(format!("`{display}` is not a display"))
                        })?
                        .color_profile = profile;
                    Ok(())
                })?;
            }
            DisplaysAction::IdentifyDisplays => self.identify()?,
            DisplaysAction::KeepDisplayChange { transaction } => self.decide(transaction, true)?,
            DisplaysAction::RevertDisplayChange { transaction } => {
                self.decide(transaction, false)?;
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
            transform: Transform::Normal,
            primary: false,
            mode: None,
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
            None,
        );
        let mut inner = displays.inner.borrow_mut();
        inner.shown = shown
            .iter()
            .enumerate()
            .map(|(index, (connector, name))| Shown {
                connector: (*connector).into(),
                monitor: (*connector).into(),
                name: (*name).into(),
                colour: DisplayCapabilities::sdr_default(),
                rect: (
                    i32::try_from(index).unwrap_or_default() * 1280,
                    0,
                    1280,
                    800,
                ),
                scale: 1.0,
                transform: "normal".into(),
                primary: index == 0,
                mode: None,
                modes: Vec::new(),
                hdr: false,
                hdr_on: false,
                sink_hdr: false,
                hdr_unavailable: None,
                sdr_brightness_nits: DEFAULT_SDR_BRIGHTNESS_NITS,
                vrr: false,
                vrr_on: true,
                sink_vrr: false,
                vrr_unavailable: None,
                colour_profile: None,
                colour_profiles: Vec::new(),
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

    tilo_test_env::scenario! {
        fn a_display_reports_its_name_its_colour_and_the_picture_prepared_for_it;

        phase a_display_reports_the_colour_its_sink_and_signal_give_it;
        {
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

        phase a_display_is_named_by_its_model_never_by_its_connector;
        {
            let placed = |name: &str, label: &str| OutputInfo {
                name: name.to_owned(),
                monitor: name.to_owned(),
                label: label.to_owned(),
                enabled: true,
                rect: Some((0, 0, 1280, 800)),
                scale: 1.0,
                transform: Transform::Normal,
                primary: false,
                mode: None,
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

        phase a_picture_on_every_display_is_prepared_for_what_they_all_show;
        {
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
    ) -> Result<(), ApiError> {
        displays.invoke(RESOURCE, action, arguments).map(drop)
    }

    tilo_test_env::scenario! {
        fn a_brightness_level_moves_the_displays_that_can_dim_and_is_refused_otherwise;

        phase the_reading_follows_a_brightness_change;
        {
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

        phase shift_moves_every_dimmable_display_together;
        {
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

        phase a_key_on_a_machine_that_dims_nothing_says_so;
        {
            let mut service = showing(None, &[("HDMI-A-1", "HDMI-A-1")]);
            let error = invoke(&mut service, "brightness-down", &Map::new()).expect_err("nothing dims");
            assert!(
                matches!(&error, ApiError::Unavailable { code, .. } if code == "no-backlight"),
                "{error:?}"
            );
            assert_eq!(reading(&service).brightness(), None);
            assert!(!reading(&service).displays[0].backlight);
        }

        phase a_level_outside_zero_to_one_is_refused;
        {
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
                assert!(matches!(error, ApiError::InvalidArguments(_)), "{error:?}");
            }
        }

        phase the_ends_of_zero_to_one_are_levels;
        {
            let dials = Dials::with(&[("eDP-1", 0.5)]);
            let mut service = showing(Some(dials.clone()), &[("eDP-1", "Built-in")]);
            for level in [0.0_f32, 1.0] {
                invoke(
                    &mut service,
                    "set-brightness",
                    &Map::from_iter([("level".to_owned(), Value::from(f64::from(level)))]),
                )
                .expect("the end of the range is a level");
                assert!(
                    close(dials.at("eDP-1"), level),
                    "the display is at {level}: {:?}",
                    dials.at("eDP-1")
                );
            }
        }

        phase a_level_for_a_display_with_nothing_to_dim_is_refused_and_moves_no_other;
        {
            let dials = Dials::with(&[("eDP-1", 0.5)]);
            let mut service = showing(
                Some(dials.clone()),
                &[("eDP-1", "Built-in"), ("HDMI-A-1", "Dell")],
            );
            let error = invoke(
                &mut service,
                "set-display-brightness",
                &Map::from_iter([
                    ("display".to_owned(), Value::from("HDMI-A-1")),
                    ("level".to_owned(), Value::from(0.9)),
                ]),
            )
            .expect_err("that display has nothing to dim");
            assert!(
                matches!(&error, ApiError::Unavailable { code, .. } if code == "unknown-display"),
                "{error:?}"
            );
            assert!(
                dials.0.borrow().set.is_empty(),
                "no display was set in its place"
            );
            assert!(close(dials.at("eDP-1"), 0.5));
        }
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

    /// A display `DP-1` that can show HDR and vary its refresh rate, offering `profiles` .
    fn tunable(hdr: bool, profiles: &[&str]) -> settings_store::hardware::DisplayHardwareSnapshot {
        use settings_store::hardware::{
            DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, OutputDescriptor,
            OutputMode,
        };
        DisplayHardwareSnapshot {
            generated_unix_ms: 0,
            backend: "headless".into(),
            publisher_pid: 1,
            publisher_start_ticks: 0,
            outputs: vec![DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: "DP-1".into(),
                    connector_label: "DP-1".into(),
                    modes: vec![OutputMode {
                        width: 2560,
                        height: 1440,
                        refresh_mhz: 60_000,
                    }],
                    hdr,
                    vrr: true,
                    color_profiles: profiles.iter().map(|id| (*id).to_owned()).collect(),
                    monitor: None,
                },
                current_mode: OutputMode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 60_000,
                },
                scale: 1.0,
                builtin: false,
                sink_hdr: hdr,
                hdr_unavailable_reason: None,
                sink_vrr: true,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: profiles
                    .iter()
                    .map(|id| ((*id).to_owned(), format!("{id} profile")))
                    .collect(),
            }],
            layout: vec![LiveOutputPlacement {
                connector: "DP-1".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            }],
        }
    }

    /// The person's settings kept for these displays.
    fn kept(
        snapshot: &settings_store::hardware::DisplayHardwareSnapshot,
        config: settings_store::hardware::OutputConfig,
    ) {
        use settings_store::hardware::{DisplayConfig, DisplaySet};
        let descriptors: Vec<_> = snapshot
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        let mut stored = DisplayConfig::default();
        stored.outputs.insert("DP-1".into(), config);
        stored
            .save_for(&DisplaySet::of_descriptors(&descriptors))
            .expect("the person's settings are kept");
    }

    tilo_test_env::scenario! {
        fn a_display_reports_the_hdr_vrr_brightness_and_colour_profile_it_runs_with;

        phase the_reading_follows_the_person_s_settings;
        {
            let _home = tilo_test_env::TestHome::new().expect("a home");
            let _settings = settings_store::in_memory::installed();
            let snapshot = tunable(true, &["icc"]);
            kept(
                &snapshot,
                settings_store::hardware::OutputConfig {
                    hdr: true,
                    sdr_brightness_nits: 300,
                    vrr: false,
                    color_profile: Some("icc".into()),
                    ..settings_store::hardware::OutputConfig::default()
                },
            );
            let displays = showing(None, &[("DP-1", "Studio display")]);
            displays.land(None, &[on("DP-1", 0)], Some(&snapshot));
            let shown = reading(&displays).displays[0].clone();
            assert!(shown.hdr && shown.hdr_on, "it shows HDR as the person chose");
            assert!(!shown.vrr_on, "and no variable refresh rate");
            assert_eq!(shown.sdr_brightness_nits, 300);
            assert_eq!(shown.colour_profile.as_deref(), Some("icc"));
            assert_eq!(
                shown.colour_profiles,
                [ColourProfile {
                    key: "icc".into(),
                    label: "icc profile".into(),
                }]
            );
        }

        phase a_display_that_could_not_show_hdr_says_so;
        {
            let _home = tilo_test_env::TestHome::new().expect("a home");
            let _settings = settings_store::in_memory::installed();
            let mut snapshot = tunable(false, &[]);
            snapshot.outputs[0].sink_hdr = true;
            snapshot.outputs[0].hdr_unavailable_reason =
                Some(settings_store::hardware::HdrUnavailable::DriverUnsupported);
            let displays = showing(None, &[("DP-1", "Studio display")]);
            displays.land(None, &[on("DP-1", 0)], Some(&snapshot));
            let shown = reading(&displays).displays[0].clone();
            assert!(!shown.hdr && shown.sink_hdr);
            assert_eq!(shown.hdr_unavailable.as_deref(), Some("driver-unsupported"));
        }
    }

    /// A session whose displays service publishes `snapshot` and keeps the person's settings in
    /// this test's home, as a session on a machine does.
    fn tunable_session(snapshot: &settings_store::hardware::DisplayHardwareSnapshot) -> Displays {
        let (pid, start_ticks) = settings_store::hardware::publisher_identity()
            .expect("this process publishes displays");
        let mut published = snapshot.clone();
        published.publisher_pid = pid;
        published.publisher_start_ticks = start_ticks;
        published.save().expect("the displays are published");
        let source = settings_store::source().expect("the person's settings are here");
        Displays::new(None, Rc::new(RefCell::new(Vec::new())), Some(source))
    }

    #[test]
    fn the_hdr_vrr_brightness_and_colour_profile_actions_stage_a_change() {
        use settings_store::hardware::{DisplayConfig, DisplaySet, OutputConfig};

        let snapshot = tunable(true, &["icc"]);
        let descriptors: Vec<_> = snapshot
            .outputs
            .iter()
            .map(|output| output.descriptor.clone())
            .collect();
        let set = DisplaySet::of_descriptors(&descriptors);
        for (action, arguments) in [
            (
                "set-display-hdr",
                Map::from_iter([
                    ("display".to_owned(), Value::from("DP-1")),
                    ("on".to_owned(), Value::Bool(true)),
                ]),
            ),
            (
                "set-display-sdr-brightness",
                Map::from_iter([
                    ("display".to_owned(), Value::from("DP-1")),
                    ("nits".to_owned(), Value::from(300)),
                ]),
            ),
            (
                "set-display-vrr",
                Map::from_iter([
                    ("display".to_owned(), Value::from("DP-1")),
                    ("on".to_owned(), Value::Bool(false)),
                ]),
            ),
            (
                "set-display-colour-profile",
                Map::from_iter([
                    ("display".to_owned(), Value::from("DP-1")),
                    ("profile".to_owned(), Value::from("icc")),
                ]),
            ),
        ] {
            // Each change waits for the person to keep it, so each is tried on its own.
            let _home = tilo_test_env::TestHome::new().expect("a home");
            let _settings = settings_store::in_memory::installed();
            let mut committed = DisplayConfig::default();
            committed
                .outputs
                .insert("DP-1".into(), OutputConfig::default());
            committed.save_for(&set).expect("the settings so far");
            let mut displays = tunable_session(&snapshot);
            displays.land(None, &[on("DP-1", 0)], Some(&snapshot));
            invoke(&mut displays, action, &arguments)
                .unwrap_or_else(|error| panic!("{action} is staged: {error:?}"));
            // What the change is tried with is the transaction's candidate.
            let transaction = settings_store::hardware::DisplayTransaction::load()
                .unwrap_or_else(|| panic!("{action} stages a change"));
            let output = &transaction.candidate.outputs["DP-1"];
            let expected = match action {
                "set-display-hdr" => output.hdr,
                "set-display-sdr-brightness" => output.sdr_brightness_nits == 300,
                "set-display-vrr" => !output.vrr,
                _ => output.color_profile.as_deref() == Some("icc"),
            };
            assert!(expected, "{action} reached the candidate: {output:?}");
            // The reading says what the change is tried with, from the next pass.
            displays.land(None, &[on("DP-1", 0)], Some(&snapshot));
            let shown = reading(&displays).displays[0].clone();
            match action {
                "set-display-hdr" => assert!(shown.hdr_on),
                "set-display-sdr-brightness" => assert_eq!(shown.sdr_brightness_nits, 300),
                "set-display-vrr" => assert!(!shown.vrr_on),
                _ => assert_eq!(shown.colour_profile.as_deref(), Some("icc")),
            }
        }
    }

    #[test]
    fn a_change_no_display_can_make_is_refused() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let _settings = settings_store::in_memory::installed();
        let snapshot = tunable(false, &[]);
        let mut displays = tunable_session(&snapshot);
        let error = invoke(
            &mut displays,
            "set-display-hdr",
            &Map::from_iter([
                ("display".to_owned(), Value::from("DP-1")),
                ("on".to_owned(), Value::Bool(true)),
            ]),
        )
        .expect_err("no HDR on this display");
        assert!(
            matches!(&error, ApiError::Unavailable { code, .. } if code == "no-hdr"),
            "{error:?}"
        );
        let error = invoke(
            &mut displays,
            "set-display-sdr-brightness",
            &Map::from_iter([
                ("display".to_owned(), Value::from("DP-1")),
                ("nits".to_owned(), Value::from(600)),
            ]),
        )
        .expect_err("brightness outside the range");
        assert!(matches!(error, ApiError::InvalidArguments(_)), "{error:?}");
        let error = invoke(
            &mut displays,
            "set-display-colour-profile",
            &Map::from_iter([
                ("display".to_owned(), Value::from("DP-1")),
                ("profile".to_owned(), Value::from("nope")),
            ]),
        )
        .expect_err("a profile it does not offer");
        assert!(matches!(error, ApiError::InvalidArguments(_)), "{error:?}");
    }

    #[test]
    fn every_display_action_names_its_own_label() {
        let published = Displays::new(None, Rc::new(RefCell::new(Vec::new())), None).resources();
        let spec = published
            .iter()
            .find(|resource| resource.id.as_str() == RESOURCE)
            .expect("the displays are published");
        for (id, label) in [
            ("set-display-hdr", "Turn high dynamic range on or off"),
            (
                "set-display-sdr-brightness",
                "Set how bright ordinary content looks in HDR",
            ),
            ("set-display-vrr", "Turn a variable refresh rate on or off"),
            (
                "set-display-colour-profile",
                "Choose a display's colour profile",
            ),
        ] {
            let action = spec
                .actions
                .iter()
                .find(|action| action.id.as_str() == id)
                .unwrap_or_else(|| panic!("the displays publish `{id}`"));
            assert_eq!(action.label, label, "{id}'s words");
        }
    }
}
