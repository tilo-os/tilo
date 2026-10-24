//! Display pane — the connected outputs, their arrangement, brightness and
//! the display comfort preferences.
//!
//! - **Displays** consumes the active compositor's private runtime snapshot.
//!   Settings never independently guesses modes or output capabilities from
//!   sysfs; the process that owns KMS is the authority. An arrangement is
//!   staged, then applied as a transaction the person keeps or reverts.
//! - **Brightness** forwards to `os_actions` (`brightnessctl`, or DDC/CI for
//!   an external monitor).
//! - **Automatic brightness, Night Light and the display scale** are the
//!   person's `display` settings; the compositor's output layer reads them.
//!
//! Every change is a [`DisplayEffect`] the area's [`DisplayModel`] carries
//! out.

pub(crate) mod shared_displays;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_clock::unix_now_ms;
use ui::time::{Duration, Instant, SystemTime};

use os_actions::Dispatcher;
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::widget::{Dropdown, Slider, Toggle, Widget};

use crate::dispatcher::dispatcher_or_real;
use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::prefs::DisplayPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, block, error_row, note, rows, section, setting_row,
    setting_row_described, value_row,
};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplaySet, DisplayTransaction,
    DisplayTransactionState, MonitorIdentity, OutputConfig, OutputDescriptor, OutputMode,
    OutputTransform, logical_extent,
};

crate::effects::effect_family!(DisplayEffect => answer);

crate::area::pane_area! {
    DisplayArea;
    fn poll(&self) {
        DisplayArea::poll(self);
    }
    fn take_host_intent(&self) -> Option<ui::widget::AppHostIntent> {
        DisplayArea::take_host_intent(self)
    }
}

/// The window's [`DisplayArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&DisplayArea> {
    app.areas().find()
}

pub(crate) mod values;

/// The Night Light switch, a comfort of every display.
pub(crate) fn night_light(cx: &PaneCx, prefs: &Followed<DisplayPrefs>) -> Item {
    cx.row(t("display-night-light"))
        .id("display.night_light")
        .of(prefs, DisplayPrefs::paths().night_light)
        .toggle()
}

/// The automatic-brightness switch, where a light sensor can drive the
/// built-in panel.
pub(crate) fn automatic_brightness(cx: &PaneCx, on: bool) -> Item {
    cx.row(t("display-auto-brightness"))
        .id("display.auto_brightness")
        .toggle_effect(
            &LocalState::new(on),
            effect(DisplayEffect::SET_AUTOMATIC_BRIGHTNESS),
        )
}

/// What Displays changes: this device's outputs and their arrangement, its
/// brightness, and the person's display comforts.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DisplayEffect {
    /// The built-in panel's brightness, from 0 to 1.
    #[effect(returns, idempotent)]
    SetBrightness { brightness: f64 },
    /// Follow the ambient light.
    #[effect(returns, idempotent)]
    SetAutomaticBrightness { on: bool },
    /// An external monitor's brightness, from 0 to 1.
    #[effect(returns, row, idempotent)]
    SetMonitorBrightness { display: String, brightness: f64 },
    /// Use a display, or turn it off. Staged until applied.
    #[effect(returns, row, idempotent)]
    UseDisplay { display: String, on: bool },
    /// Make a display the one panels and new windows open on.
    #[effect(returns, row, idempotent, label = "Make primary")]
    SetPrimaryDisplay { display: String },
    /// A display's scale. Staged until applied.
    #[effect(returns, row, idempotent)]
    SetOutputScale { display: String, scale: String },
    /// Place a display beside another: `Right of …`, `Left of …`, `Above …`,
    /// `Below …`. Staged until applied.
    #[effect(returns, row)]
    PlaceDisplay { display: String, position: String },
    /// Put a display's top-left corner at a point of the desktop, as
    /// dragging it does. Staged until applied.
    #[effect(returns, row)]
    MoveDisplay { display: String, x: i32, y: i32 },
    /// Request this peer's screens at a shared logical position. No input grant.
    #[effect(returns, row, label = "Move device display")]
    MoveDeviceDisplay {
        display: String,
        revision: u64,
        x: i32,
        y: i32,
    },
    /// Inspect the observed displays and their shared arrangement, including offline devices.
    #[effect(view, returns, kind = "read", label = "Inspect shared displays")]
    InspectSharedDisplays,
    /// Include a display in the shared layout. This does not allow remote input.
    #[effect(returns, row, idempotent)]
    IncludeSharedDisplay {
        device: String,
        output: String,
        revision: u64,
        on: bool,
    },
    /// Set a display's one-based position in the person's display order.
    #[effect(returns, row, idempotent)]
    OrderSharedDisplay {
        device: String,
        output: String,
        revision: u64,
        order: u32,
    },
    /// Show another display's picture, or extend the workspace. Staged
    /// until applied.
    #[effect(returns, row)]
    MirrorDisplay { display: String, mirror: String },
    /// A display's resolution and refresh rate. Staged until applied.
    #[effect(returns, row, idempotent)]
    SetDisplayMode { display: String, mode: String },
    /// A display's rotation. Staged until applied.
    #[effect(returns, row, idempotent)]
    RotateDisplay { display: String, rotation: String },
    /// High dynamic range on a display that offers it. Staged until applied.
    #[effect(returns, row, idempotent)]
    SetHdr { display: String, on: bool },
    /// How bright SDR content is while a display runs in HDR, from 0 to 1.
    #[effect(returns, row, idempotent)]
    SetSdrBrightness { display: String, brightness: f64 },
    /// Variable refresh rate on a display that offers it. Staged until
    /// applied.
    #[effect(returns, row, idempotent)]
    SetVrr { display: String, on: bool },
    /// A display's colour profile, or `System default`. Staged until applied.
    #[effect(returns, row, idempotent)]
    SetColorProfile { display: String, profile: String },
    /// Read the compositor observations, staged configuration and preview transaction.
    #[effect(view, returns, kind = "read", label = "Inspect display configuration")]
    InspectDisplayConfiguration,
    /// Show each display's number and name on it for a few seconds.
    #[effect(returns, label = "Identify displays")]
    IdentifyDisplays,
    /// Preview the staged arrangement; it is kept only when confirmed.
    #[effect(returns, label = "Apply display changes")]
    ApplyDisplayChanges,
    /// Keep the arrangement being previewed.
    #[effect(returns, label = "Keep display changes")]
    KeepDisplayChanges,
    /// Go back to the arrangement before the preview.
    #[effect(returns, label = "Revert display changes")]
    RevertDisplayChanges,
    /// Ask again before keeping display changes from a layout tool.
    #[effect(returns, row, label = "Ask again")]
    ForgetDisplayTool {
        #[param("The tool's executable")]
        tool: String,
    },
    /// Opt this device into automatic grants to its enrolled online input devices.
    /// The separate Continuity master and Display/input switches must also be on.
    #[effect(returns, idempotent)]
    SetAutomaticDisplayInput { on: bool },
}

/// What staging a display change answers; the page shows the staged
/// change in its own card rather than as a notice.
const STAGED: &str = "Display change staged. Apply it to preview the new configuration.";

/// Answer `effect` on the area's model.
#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DisplayEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if matches!(
        effect,
        DisplayEffect::InspectDisplayConfiguration
            | DisplayEffect::IdentifyDisplays
            | DisplayEffect::ApplyDisplayChanges
            | DisplayEffect::KeepDisplayChanges
            | DisplayEffect::RevertDisplayChanges
    ) && crate::panes::display::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow()
        .of_device
    {
        let command = crate::panes::display::area(app)
            .ok_or_else(crate::area::missing)?
            .model()
            .borrow()
            .host_command(&effect)?;
        let inspect = effect == DisplayEffect::InspectDisplayConfiguration;
        // Only a delivered invocation can name broker-retained original custody.
        // Physical UI effects have no remote invoker and use the certified local channel.
        let original = if ui::invocation_scope::current_invoker().is_some() {
            ui::collaboration::DisplayOrigin::Published(
                cx.invocation()
                    .ok_or("The original display invocation is unavailable.")?
                    .0
                    .into_bytes(),
            )
        } else {
            ui::collaboration::DisplayOrigin::LocalHost
        };
        return Ok(crate::device_read::job(
            cx,
            move || request_host_display(&command, original),
            move |app, result, _| {
                let model = crate::panes::display::area(app)
                    .ok_or_else(crate::area::missing)?
                    .model();
                let mut model = model.borrow_mut();
                model.accept_host_reply(result?);
                let value = if inspect {
                    model.inspection()
                } else {
                    let transaction = model
                        .host_state
                        .as_ref()
                        .and_then(|state| state.transaction.as_ref());
                    serde_json::json!({
                        "transaction": transaction.map(|value| value.id),
                        "state": transaction.map(|value| value.state),
                        "identify_requested_unix_ms": model.host_state.as_ref().and_then(|state| state.identify.as_ref()).map(|request| request.requested_unix_ms),
                    })
                };
                Ok(Effected::value(value))
            },
        ));
    }
    if effect == DisplayEffect::InspectSharedDisplays {
        return Ok(Effected::value(
            crate::panes::display::area(app)
                .ok_or_else(crate::area::missing)?
                .model()
                .borrow()
                .shared_display_inspection(),
        ));
    }
    if effect == DisplayEffect::InspectDisplayConfiguration {
        return Ok(Effected::value(
            crate::panes::display::area(app)
                .ok_or_else(crate::area::missing)?
                .model()
                .borrow()
                .inspection(),
        ));
    }
    let staged = {
        let model = crate::panes::display::area(app)
            .ok_or_else(crate::area::missing)?
            .model();
        let mut model = model.borrow_mut();
        let staged = model.apply(&effect)?;
        if staged == STAGED {
            model.notice = None;
        }
        staged
    };
    // A change that cannot leave a screen dark applies as it is chosen;
    // only one that can is tried with Keep and Revert first.
    if crate::panes::display::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow()
        .of_device
        && crate::panes::display::area(app)
            .ok_or_else(crate::area::missing)?
            .model()
            .borrow()
            .staged_applies_at_once()
    {
        return answer(app, DisplayEffect::ApplyDisplayChanges, cx);
    }
    Ok(Effected::value(staged))
}

fn request_host_display(
    command: &settings_store::display_control::DisplayCommand,
    original: ui::collaboration::DisplayOrigin,
) -> Result<settings_store::display_control::DisplayReply, String> {
    let bytes =
        serde_json::to_vec(command).map_err(|_| "The display request could not be encoded.")?;
    let reply = ui::collaboration::display_control(bytes, original)?;
    serde_json::from_slice(&reply)
        .map_err(|_| "The display service returned an invalid reply.".into())
}

const NIGHT_LIGHT_TEMPERATURES: &[(&str, u32)] = &[
    ("Subtle, 4500 K", 4_500),
    ("Warm, 3700 K", 3_700),
    ("Very warm, 2800 K", 2_800),
];

fn temperature_ftl(english: &str) -> &'static str {
    match english {
        "Subtle, 4500 K" => "display-night-light-temp-subtle",
        "Very warm, 2800 K" => "display-night-light-temp-very-warm",
        _ => "display-night-light-temp-warm",
    }
}

fn temperature_shown(english: &str) -> String {
    t(temperature_ftl(english))
}

fn temperature_label(kelvin: u32) -> String {
    let english = NIGHT_LIGHT_TEMPERATURES
        .iter()
        .min_by_key(|(_, value)| value.abs_diff(kelvin))
        .map_or("Warm, 3700 K", |(label, _)| *label);
    temperature_shown(english)
}

fn temperature_from_label(label: &str) -> Option<u32> {
    NIGHT_LIGHT_TEMPERATURES
        .iter()
        .find(|(english, _)| *english == label || temperature_shown(english) == label)
        .map(|(_, value)| *value)
}

fn temperature_options() -> Vec<String> {
    NIGHT_LIGHT_TEMPERATURES
        .iter()
        .map(|(english, _)| temperature_shown(english))
        .collect()
}

/// Offered `HiDPI` display scales as `(display label, factor)`. The label is
/// what the dropdown shows and stores in the CRDT; the factor is what
/// [`DisplayPrefs::scale`] persists and the compositor advertises as the
/// preferred `wp_fractional_scale`.
pub const DISPLAY_SCALES: &[(&str, f64)] = &[
    ("100%", 1.0),
    ("125%", 1.25),
    ("150%", 1.5),
    ("175%", 1.75),
    ("200%", 2.0),
];

/// Map a scale factor to the closest offered label (defaults to "100%").
pub(crate) fn scale_label(scale: f64) -> &'static str {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited settings.json
    // could hold a non-finite scale, and this must degrade to "100%" rather
    // than panic the pane.
    DISPLAY_SCALES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("100%", |(label, _)| *label)
}

/// Map a scale label back to its factor, if recognised.
fn scale_factor_of(label: &str) -> Option<f64> {
    DISPLAY_SCALES
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, s)| *s)
}

pub(crate) fn output_key(connector: &str, field: &str) -> String {
    format!("display.output.{connector}.{field}")
}

const TRANSFORMS: &[(&str, OutputTransform)] = &[
    ("Landscape", OutputTransform::Normal),
    ("Portrait right", OutputTransform::Rotate90),
    ("Upside down", OutputTransform::Rotate180),
    ("Portrait left", OutputTransform::Rotate270),
];

fn transform_ftl(english: &str) -> &'static str {
    match english {
        "Portrait right" => "display-rotation-portrait-right",
        "Upside down" => "display-rotation-upside-down",
        "Portrait left" => "display-rotation-portrait-left",
        _ => "display-rotation-landscape",
    }
}

fn transform_shown(english: &str) -> String {
    t(transform_ftl(english))
}

pub(crate) fn transform_label(transform: OutputTransform) -> String {
    let english = TRANSFORMS
        .iter()
        .find(|(_, candidate)| *candidate == transform)
        .map_or("Landscape", |(label, _)| *label);
    transform_shown(english)
}

fn transform_from_label(label: &str) -> Option<OutputTransform> {
    TRANSFORMS
        .iter()
        .find(|(english, _)| *english == label || transform_shown(english) == label)
        .map(|(_, transform)| *transform)
}

fn transform_options() -> Vec<String> {
    TRANSFORMS
        .iter()
        .map(|(english, _)| transform_shown(english))
        .collect()
}

/// How a person reads a mode's resolution: `2560 × 1440`.
fn resolution_label(mode: OutputMode) -> String {
    resolution_text(mode.width, mode.height)
}

/// A resolution as people write it: `2560 × 1440`.
fn resolution_text(width: u32, height: u32) -> String {
    // ratchet-allow(glyph_icon): the multiplication sign of a resolution is text, not an icon
    format!("{width} × {height}")
}

/// How a person reads a mode's refresh rate: `144 Hz`, `59.94 Hz`.
fn refresh_label(mode: OutputMode) -> String {
    let label = mode.label();
    label
        .split_once(" at ")
        .map_or_else(|| label.clone(), |(_, rate)| rate.to_owned())
}

/// The resolutions among `modes`, largest first, each once.
fn resolution_choices(modes: &[OutputMode]) -> Vec<String> {
    let mut sizes: Vec<(u32, u32)> = Vec::new();
    for mode in modes {
        if !sizes.contains(&(mode.width, mode.height)) {
            sizes.push((mode.width, mode.height));
        }
    }
    sizes.sort_by_key(|&(width, height)| std::cmp::Reverse(u64::from(width) * u64::from(height)));
    sizes
        .into_iter()
        .map(|(width, height)| resolution_text(width, height))
        .collect()
}

/// The refresh rates `modes` offer at `current`'s resolution, fastest first.
fn refresh_choices(modes: &[OutputMode], current: OutputMode) -> Vec<String> {
    let mut rates: Vec<OutputMode> = modes
        .iter()
        .copied()
        .filter(|mode| (mode.width, mode.height) == (current.width, current.height))
        .collect();
    rates.sort_by_key(|mode| std::cmp::Reverse(mode.refresh_mhz));
    rates.dedup_by_key(|mode| mode.refresh_mhz);
    rates.into_iter().map(refresh_label).collect()
}

/// The mode `resolution` (as [`resolution_label`] reads) names: at
/// `current`'s refresh rate where it has it, else at its fastest.
fn mode_for_resolution(
    modes: &[OutputMode],
    current: OutputMode,
    resolution: &str,
) -> Option<OutputMode> {
    let sized = modes
        .iter()
        .copied()
        .filter(|mode| resolution_label(*mode) == resolution);
    sized
        .clone()
        .find(|mode| mode.refresh_mhz == current.refresh_mhz)
        .or_else(|| sized.max_by_key(|mode| mode.refresh_mhz))
}

/// The mode at `current`'s resolution refreshing at `rate` (as
/// [`refresh_label`] reads).
fn mode_for_refresh(modes: &[OutputMode], current: OutputMode, rate: &str) -> Option<OutputMode> {
    modes.iter().copied().find(|mode| {
        (mode.width, mode.height) == (current.width, current.height) && refresh_label(*mode) == rate
    })
}

/// Localise the built-in panel name for chrome. The stored label stays
/// `Built-in display` so placement strings and tests keep one identifier.
fn output_title(output: &DisplayOutput) -> String {
    if output.builtin {
        output
            .label
            .replacen("Built-in display", &t("display-builtin"), 1)
    } else {
        output.label.clone()
    }
}

/// Best pair of timings which address the same pixel extent. The compositor
/// duplicates the viewport in software, so connector refresh rates may differ
/// (common for a nominal 60 Hz USB-C monitor reporting 59.94 Hz).
fn best_mirror_modes(
    source: &DisplayOutput,
    target: &DisplayOutput,
) -> Option<(OutputMode, OutputMode)> {
    let (width, height) = source
        .modes
        .iter()
        .filter(|source_mode| {
            target.modes.iter().any(|target_mode| {
                target_mode.width == source_mode.width && target_mode.height == source_mode.height
            })
        })
        .map(|mode| (mode.width, mode.height))
        .max_by_key(|&(width, height)| (u64::from(width) * u64::from(height), width, height))?;
    let best_for = |output: &DisplayOutput| {
        output
            .modes
            .iter()
            .filter(|mode| mode.width == width && mode.height == height)
            .copied()
            .max_by_key(|mode| mode.refresh_mhz)
    };
    Some((best_for(source)?, best_for(target)?))
}

/// The desk monitor the displays captures show beside the panel.
const SAMPLE_MONITOR: &str = "DP-1";

/// A 27-inch QHD monitor on `DP-1`, for the displays captures.
fn sample_monitor() -> DisplayOutput {
    let modes = [144_000, 120_000, 60_000]
        .into_iter()
        .map(|refresh_mhz| OutputMode {
            width: 2560,
            height: 1440,
            refresh_mhz,
        })
        .chain([OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        }])
        .collect::<Vec<_>>();
    let descriptor = OutputDescriptor {
        connector: SAMPLE_MONITOR.into(),
        connector_label: SAMPLE_MONITOR.into(),
        modes: modes.clone(),
        hdr: true,
        vrr: true,
        color_profiles: Vec::new(),
        monitor: Some(MonitorIdentity {
            make: "Dell".into(),
            model: "DELL U2724DE".into(),
            ..MonitorIdentity::default()
        }),
    };
    DisplayOutput {
        label: settings_store::display_identify::display_name(&descriptor, false),
        connected: true,
        builtin: false,
        connector: SAMPLE_MONITOR.into(),
        connector_label: SAMPLE_MONITOR.into(),
        current_mode: modes.first().copied(),
        modes,
        scale: 1.0,
        hdr: true,
        sink_hdr: true,
        hdr_unavailable_reason: None,
        vrr: true,
        sink_vrr: true,
        vrr_unavailable_reason: None,
        unavailable_reason: None,
        color_profiles: Vec::new(),
        color_profile_labels: BTreeMap::new(),
        monitor: descriptor.monitor,
    }
}

/// One real display output, derived from a DRM connector.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors an independent DRM/compositor capability flag (connected, \
              builtin, hdr, sink_hdr, vrr, sink_vrr); they are not related toggles that could \
              collapse into an enum"
)]
pub struct DisplayOutput {
    /// User-facing name, e.g. `"Built-in display"` or `"HDMI-A-1"`.
    pub label: String,
    pub connected: bool,
    pub builtin: bool,
    /// Collision-free output key used by the compositor-facing persistence
    /// contract. It is card-qualified only for duplicate raw labels.
    pub connector: String,
    /// Raw KMS connector label, retained separately from [`Self::label`]
    /// because the UI appends a card qualifier to duplicate labels.
    pub connector_label: String,
    /// Modes discovered from sysfs/compositor status. An empty list means
    /// mode selection is unavailable, not that a made-up default is valid.
    pub modes: Vec<OutputMode>,
    pub current_mode: Option<OutputMode>,
    /// The scale the compositor shows it at; an output with no saved entry
    /// runs at the compositor's own choice.
    pub scale: f64,
    pub hdr: bool,
    pub sink_hdr: bool,
    pub hdr_unavailable_reason: Option<settings_store::hardware::HdrUnavailable>,
    pub vrr: bool,
    pub sink_vrr: bool,
    pub vrr_unavailable_reason: Option<settings_store::hardware::AdaptiveSyncUnavailable>,
    /// Why this connected display is dark although it is on, as the
    /// compositor reported it.
    pub unavailable_reason: Option<settings_store::hardware::OutputUnavailable>,
    pub color_profiles: Vec<String>,
    pub color_profile_labels: BTreeMap<String, String>,
    /// EDID identity; decides which saved arrangement applies.
    pub monitor: Option<MonitorIdentity>,
}

/// The Fluent key explaining why a connected display stays dark.
fn output_unavailable_key(reason: settings_store::hardware::OutputUnavailable) -> &'static str {
    use settings_store::hardware::OutputUnavailable;
    match reason {
        OutputUnavailable::LinkDown => "display-link-down",
        OutputUnavailable::DriverRejected => "display-output-driver-rejected",
    }
}

/// The Fluent key explaining why HDR cannot be used.
fn hdr_unavailable_key(
    sink_hdr: bool,
    reason: Option<settings_store::hardware::HdrUnavailable>,
) -> &'static str {
    use settings_store::hardware::HdrUnavailable;
    match reason {
        Some(HdrUnavailable::DriverUnsupported) => "display-hdr-driver-unsupported",
        Some(HdrUnavailable::FormatUnsupported) => "display-hdr-format-unsupported",
        Some(HdrUnavailable::DriverRejected) => "display-hdr-driver-rejected",
        None if sink_hdr => "display-hdr-driver-unsupported",
        None => "display-hdr-unsupported",
    }
}

/// SDR brightness in cd/m² as the slider's `0.0..=1.0` position.
fn sdr_brightness_fraction(nits: u16) -> f64 {
    let range = settings_store::hardware::SDR_BRIGHTNESS_NITS_RANGE;
    let (low, high) = (f64::from(*range.start()), f64::from(*range.end()));
    ((f64::from(nits) - low) / (high - low)).clamp(0.0, 1.0)
}

/// The slider's `0.0..=1.0` position as SDR brightness in whole cd/m².
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into the u16 brightness range before the cast"
)]
fn sdr_brightness_nits(fraction: f64) -> u16 {
    let range = settings_store::hardware::SDR_BRIGHTNESS_NITS_RANGE;
    let (low, high) = (f64::from(*range.start()), f64::from(*range.end()));
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    fraction.mul_add(high - low, low).round().clamp(low, high) as u16
}

/// The Fluent key explaining why variable refresh rate cannot be used.
fn adaptive_sync_unavailable_key(
    sink_vrr: bool,
    reason: Option<settings_store::hardware::AdaptiveSyncUnavailable>,
) -> &'static str {
    use settings_store::hardware::AdaptiveSyncUnavailable;
    match reason {
        Some(AdaptiveSyncUnavailable::DriverUnsupported) => "display-vrr-driver-unsupported",
        Some(AdaptiveSyncUnavailable::DriverRejected) => "display-vrr-driver-rejected",
        None if sink_vrr => "display-vrr-driver-unsupported",
        None => "display-vrr-unsupported",
    }
}

impl DisplayOutput {
    /// The entry that describes this output as it runs with none saved.
    fn unconfigured(&self) -> OutputConfig {
        OutputConfig::at_scale(self.scale)
    }
}

/// An output's extent in the logical layout its `x`/`y` are stored in: the
/// staged mode (else the current one) over the staged scale, rotated.
fn logical_extent_of(output: &DisplayOutput, cfg: &OutputConfig) -> (i32, i32) {
    let mode = cfg
        .mode
        .or(output.current_mode)
        .map_or((1920, 1080), |mode| (mode.width, mode.height));
    let (width, height) = logical_extent(mode, cfg.scale, cfg.transform);
    (
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    )
}

/// Whether a DRM connector directory name is a *virtual* connector rather
/// than a user-facing display port. The compositor's writeback connector
/// (`card0-Writeback-1`) and any `Virtual`/`None` connectors are not physical
/// outputs a user plugs a monitor into, so they must not appear in the
/// arrangement list.
#[cfg(test)]
fn is_virtual_connector(conn: &str) -> bool {
    let lower = conn.to_ascii_lowercase();
    lower.contains("writeback") || lower.contains("virtual")
}

/// Read capabilities only from the active compositor's leased snapshot;
/// `None` when there is no snapshot to read (the compositor has not
/// published one, or it is gone), which is not the same as no displays.
fn read_outputs() -> Option<Vec<DisplayOutput>> {
    let outputs = DisplayHardwareSnapshot::load().map(|snapshot| outputs_from_snapshot(&snapshot));
    if outputs.is_none() {
        tracing::warn!(
            path = ?settings_store::hardware::display_snapshot_path(),
            "the compositor's display snapshot is missing or not live"
        );
    }
    outputs
}

fn outputs_from_snapshot(snapshot: &DisplayHardwareSnapshot) -> Vec<DisplayOutput> {
    // The names Identify shows on the displays: the model, and the number
    // too where two connected displays share one.
    let labels = settings_store::display_identify::display_names(&snapshot.outputs);
    let mut out: Vec<_> = snapshot
        .outputs
        .iter()
        .cloned()
        .zip(labels)
        .map(|(snapshot, label)| {
            let connector_label = snapshot.descriptor.label().to_string();
            let connector = snapshot.descriptor.connector;
            DisplayOutput {
                label,
                connected: true,
                builtin: snapshot.builtin,
                connector,
                connector_label,
                modes: snapshot.descriptor.modes,
                current_mode: Some(snapshot.current_mode),
                scale: snapshot.scale,
                hdr: snapshot.descriptor.hdr,
                sink_hdr: snapshot.sink_hdr,
                hdr_unavailable_reason: snapshot.hdr_unavailable_reason,
                vrr: snapshot.descriptor.vrr,
                sink_vrr: snapshot.sink_vrr,
                vrr_unavailable_reason: snapshot.vrr_unavailable_reason,
                unavailable_reason: snapshot.unavailable_reason,
                color_profiles: snapshot.descriptor.color_profiles,
                color_profile_labels: snapshot.color_profile_labels,
                monitor: snapshot.descriptor.monitor,
            }
        })
        .collect();
    // Deterministic order: built-in first, then connected, then alphabetical.
    out.sort_by(|a, b| {
        b.builtin
            .cmp(&a.builtin)
            .then(b.connected.cmp(&a.connected))
            .then(a.label.cmp(&b.label))
    });
    out
}

/// External monitors among `outputs` whose brightness the system can set,
/// keyed by output key, with their current level.
fn read_output_brightness(outputs: &[DisplayOutput]) -> BTreeMap<String, f32> {
    if !outputs
        .iter()
        .any(|output| output.connected && !output.builtin)
    {
        return BTreeMap::new();
    }
    output_brightness_from(outputs, &os_actions::BrightnessService::global().levels())
}

/// The external monitors among `outputs` that are dimmed in software.
fn read_dimmed_by_tilo(outputs: &[DisplayOutput]) -> BTreeSet<String> {
    dimmed_by_tilo_from(outputs, &os_actions::BrightnessService::global().levels())
}

fn dimmed_by_tilo_from(
    outputs: &[DisplayOutput],
    levels: &[os_actions::MonitorLevel],
) -> BTreeSet<String> {
    outputs
        .iter()
        .filter(|output| output.connected && !output.builtin)
        .filter(|output| {
            levels.iter().any(|level| {
                level.connector == output.connector_label
                    && level.kind == os_actions::BrightnessKind::Software
            })
        })
        .map(|output| output.connector.clone())
        .collect()
}

fn output_brightness_from(
    outputs: &[DisplayOutput],
    levels: &[os_actions::MonitorLevel],
) -> BTreeMap<String, f32> {
    outputs
        .iter()
        .filter(|output| output.connected && !output.builtin)
        .filter_map(|output| {
            levels
                .iter()
                .find(|level| level.connector == output.connector_label)
                .map(|level| (output.connector.clone(), level.level))
        })
        .collect()
}

/// The panel's backlight and external monitors' brightness.
pub trait BrightnessControl: Send + Sync {
    /// # Errors
    /// Why the panel's brightness could not be set.
    fn set_brightness(&self, percent: f32) -> Result<(), String>;
    /// # Errors
    /// Why the monitor on `connector` could not be set.
    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String>;
}

/// Brightness through the system's dispatcher.
struct SystemBrightness(Arc<dyn Dispatcher + Send + Sync>);

impl BrightnessControl for SystemBrightness {
    fn set_brightness(&self, percent: f32) -> Result<(), String> {
        self.0.set_brightness(percent)
    }
    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
        self.0.set_display_brightness(connector, percent)
    }
}

/// Brightness that changes nothing, for a screenshot and the crate's tests.
struct NoBrightness;

impl BrightnessControl for NoBrightness {
    fn set_brightness(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_display_brightness(&self, _: &str, _: f32) -> Result<(), String> {
        Ok(())
    }
}

/// The panel brightness (or why it could not be read) and whether an
/// ambient light sensor is present, as the background probe found them.
type HardwareProbe = (Result<f32, String>, bool);

/// The session's pending display change (its `display_transaction`
/// section), read through the settings document Settings reaches over its
/// channel to the shell: the host writes it, Settings only reads it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct PendingDisplayChange(serde_json::Map<String, serde_json::Value>);

impl ui::storage::SettingsSchema for PendingDisplayChange {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("display_transaction");
}

impl PendingDisplayChange {
    /// The transaction the section holds, if it holds one.
    fn transaction(&self) -> Option<DisplayTransaction> {
        if !self.0.contains_key("id") {
            return None;
        }
        serde_json::from_value(serde_json::Value::Object(self.0.clone())).ok()
    }
}

/// What the Displays area keeps for the window: the model, read when first
/// needed.
pub struct DisplayArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<DisplayModel>>,
}

impl DisplayArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model over this device's displays, or over a sample panel for a
    /// screenshot.
    pub fn model(&self) -> &Shared<DisplayModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                DisplayModel::of_device(self.cx.clone())
            } else {
                DisplayModel::sample(self.cx.clone())
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// The model, where the displays were read; never reads them.
    pub(crate) fn read(&self) -> Option<&Shared<DisplayModel>> {
        self.model.get()
    }

    /// Take in background results, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            let mut model = model.borrow_mut();
            model.poll_device_move();
            model.poll_background();
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// What the host is asked to do for a display move. The host asks on
    /// every turn of its event loop, the first one before Settings' first
    /// frame, so asking never reads the displays: a move is only ever
    /// pending in a model an effect has already read.
    pub(crate) fn take_host_intent(&self) -> Option<ui::widget::AppHostIntent> {
        self.model.get()?.borrow_mut().take_host_intent()
    }

    /// One row per connected display and per trusted layout tool.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: DisplayEffect| SettingsEffect::from(effect).bound();
        let mut rows: Vec<LiveRow> = model
            .connected_outputs()
            .into_iter()
            .map(|output| model.output_row(output))
            .collect();
        rows.extend(
            model
                .device_displays
                .iter()
                .filter(|display| DisplayModel::device_movable(display))
                .map(DisplayModel::device_output_row),
        );
        rows.extend(model.shared_display_rows());
        if model.of_device {
            for tool in &settings_store::display_tools::TrustedDisplayTools::load().tools {
                rows.push(
                    LiveRow::new(
                        format!("settings.display-tool:{}", tool.executable),
                        "settings.display-tool",
                        tool.name().to_owned(),
                    )
                    .operation(offer(DisplayEffect::ForgetDisplayTool {
                        tool: tool.executable.clone(),
                    })),
                );
            }
        }
        rows
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: whose displays, what the hardware offers, what awaits an answer, whether it changed"
)]
pub struct DisplayModel {
    cx: PaneCx,
    /// Whether the outputs, arrangement and transactions are this device's.
    of_device: bool,
    brightness_control: Arc<dyn BrightnessControl>,
    outputs: Vec<DisplayOutput>,
    /// Whether the compositor's display snapshot could be read: without
    /// it the pane cannot tell what is connected, which it must not show
    /// as "no displays".
    outputs_read: bool,
    device_displays: Vec<settings_store::device_displays::DeviceDisplay>,
    person_devices: Vec<settings_store::person_displays::PersonDevice>,
    shared_display_prefs: Followed<shared_displays::SharedDisplayPrefs>,
    automatic_display_prefs: Followed<shared_displays::AutomaticDisplayPrefs>,
    /// The display change being tried, as the settings document shows it.
    pending_change: Followed<PendingDisplayChange>,
    pending_device_move: Option<settings_store::device_displays::DeviceDisplayMove>,
    awaiting_device_move: Option<(settings_store::device_displays::DeviceDisplayMove, Instant)>,
    shared_move_revisions: Option<Vec<(String, u64)>>,
    device_move_reply: Option<Rc<std::cell::RefCell<ui::widget::DeviceDisplayMoveReply>>>,
    ambient_sensor_available: bool,
    brightness: f32,
    prefs: Followed<DisplayPrefs>,
    output_config: DisplayConfig,
    /// External monitors whose brightness is set over DDC/CI, by output key,
    /// with their last known level. The built-in panel's backlight is the
    /// Brightness card's slider.
    output_brightness: BTreeMap<String, f32>,
    /// The monitors among [`Self::output_brightness`] with no hardware
    /// control, which the compositor darkens instead: their slider says so.
    dimmed_by_tilo: BTreeSet<String>,
    pending_candidate: Option<DisplayConfig>,
    last_seen_transaction: Option<(u64, DisplayTransactionState)>,
    host_state: Option<settings_store::display_control::DisplayReply>,
    host_poll_deadline: Option<Instant>,
    host_probe: Option<
        crate::channel::Receiver<Result<settings_store::display_control::DisplayReply, String>>,
    >,
    last_snapshot_check: Instant,
    snapshot_stamp: Option<SystemTime>,
    /// While a pane shows the displays: the watch on the compositor's
    /// snapshots, which says when to read them again.
    following: std::rc::Weak<crate::watch::Watched>,
    /// A display change awaits the compositor or the person; its countdown
    /// is looked at each second.
    settling: bool,
    /// The brightness service finished the monitor scan this model asked
    /// for, so monitors that speak DDC/CI can gain their sliders.
    levels_landed: Arc<AtomicBool>,
    hardware_probe: Option<crate::channel::Receiver<HardwareProbe>>,
    /// Why the panel brightness could not be read; the slider is then not
    /// offered at a made-up level.
    brightness_error: Option<String>,
    brightness_tx: Option<crate::channel::Sender<f32>>,
    brightness_result_rx: Option<crate::channel::Receiver<Result<f32, String>>>,
    pending_brightness: Option<f32>,
    /// What the last change, or the compositor's answer to it, said.
    notice: Option<String>,
    /// The arrangement canvas's drag and settle; kept here because the
    /// composition is rebuilt between frames.
    arrangement_drag: LocalState<super::display_arrangement::ArrangementGesture>,
    device_arrangement_drag: LocalState<super::display_arrangement::ArrangementGesture>,
    /// The one selected display, by output key: chosen on the canvas (by
    /// pointer, touch, keys or screen reader). Its settings are the page's.
    selected: LocalState<String>,
    /// The revision of [`Self::selected`] the page shows.
    selected_seen: u64,
    /// The display under the pointer on the canvas.
    pointed: LocalState<Option<String>>,
    pointed_seen: u64,
    /// The display whose number was last flashed on it.
    last_flash: Option<String>,
    /// While Identify shows the numbers: when it showed and goes.
    identify_window: Option<(Instant, Instant)>,
    /// What the capture shows of the canvas, for screenshots.
    sample: Option<crate::scenarios::DisplaysSample>,
    changed: bool,
}

impl DisplayModel {
    fn device_movable(display: &settings_store::device_displays::DeviceDisplay) -> bool {
        display.online && !display.local && display.revision != 0
    }

    fn device_output_row(display: &settings_store::device_displays::DeviceDisplay) -> LiveRow {
        LiveRow::new(
            format!("settings.device-display:{}", display.key()),
            "settings.device-display",
            display.label.clone(),
        )
        .operation(
            SettingsEffect::from(DisplayEffect::MoveDeviceDisplay {
                display: display.key(),
                revision: display.revision,
                x: 0,
                y: 0,
            })
            .bound_asking(&["x", "y"]),
        )
    }

    pub(crate) fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        let request = self.pending_device_move.take()?;
        let reply = Rc::downgrade(self.device_move_reply.as_ref()?);
        Some(ui::widget::AppHostIntent::MoveDeviceDisplay { request, reply })
    }

    fn poll_device_move(&mut self) {
        let result = self
            .device_move_reply
            .as_ref()
            .and_then(|reply| reply.borrow_mut().result.take());
        if let Some(result) = result {
            self.device_move_reply = None;
            self.notice = Some(result.map_or_else(
                |error| {
                    self.awaiting_device_move = None;
                    error
                },
                |()| t("display-device-move-requested"),
            ));
            self.changed = true;
        }
        if self
            .awaiting_device_move
            .as_ref()
            .is_some_and(|(_, started)| started.elapsed() >= Duration::from_secs(10))
        {
            self.device_move_reply = None;
            self.pending_device_move = None;
            self.awaiting_device_move = None;
            self.notice = Some(t("display-device-move-unconfirmed"));
            self.changed = true;
            return;
        }
        if self.device_move_reply.is_some() {
            return;
        }
        let Some((request, _started)) = &self.awaiting_device_move else {
            return;
        };
        let observed = self
            .device_displays
            .iter()
            .find(|screen| screen.key() == request.screen.key());
        let request = request.clone();
        let confirmed = observed.is_some_and(|screen| {
            screen.online
                && screen.revision > request.screen.revision
                && (screen.x, screen.y) == (request.x, request.y)
        });
        let message = match observed {
            None => Some(t("display-device-move-unavailable")),
            Some(screen) if !screen.online => Some(t("display-device-move-unavailable")),
            Some(screen) if screen.revision > request.screen.revision => {
                Some(t(if (screen.x, screen.y) == (request.x, request.y) {
                    "display-device-move-confirmed"
                } else {
                    "display-device-move-changed"
                }))
            }
            _ => None,
        };
        if let Some(mut message) = message {
            if confirmed && let Err(error) = self.confirm_shared_device_move(&request.screen.device)
            {
                message = error;
            }
            self.shared_move_revisions = None;
            self.awaiting_device_move = None;
            self.notice = Some(message);
            self.changed = true;
        }
    }

    /// This device's displays; brightness is probed and set off the UI
    /// thread.
    fn of_device(cx: PaneCx) -> Self {
        let (probe_tx, probe_rx) = crate::channel::channel();
        let brightness_error = crate::device_read::thread(
            "settings-display-probe",
            Box::new(move || {
                let _ = probe_tx.send((
                    os_actions::get_brightness(),
                    os_actions::has_ambient_light_sensor(),
                ));
            }),
        )
        .err()
        .map(|error| format!("the brightness probe could not start: {error}"));
        let control: Arc<dyn BrightnessControl> =
            Arc::new(SystemBrightness(dispatcher_or_real(None)));
        let (brightness_tx, brightness_rx) = crate::channel::channel::<f32>();
        let (result_tx, result_rx) = crate::channel::channel();
        let worker = Arc::clone(&control);
        crate::device_read::thread(
            "settings-brightness-apply",
            Box::new(move || {
                while let Ok(percent) = brightness_rx.recv() {
                    let _ =
                        result_tx.send(worker.set_brightness(percent).map(|()| percent / 100.0));
                }
            }),
        )
        .ok();
        let read = read_outputs();
        let outputs_read = read.is_some();
        let outputs = read.unwrap_or_default();
        let output_config = DisplayConfig::load_for(&Self::display_set_of(&outputs));
        let output_brightness = read_output_brightness(&outputs);
        let dimmed_by_tilo = read_dimmed_by_tilo(&outputs);
        let levels_landed = Arc::new(AtomicBool::new(false));
        let landed = Arc::clone(&levels_landed);
        let wake = ui::WakeHandle::new();
        os_actions::BrightnessService::global().when_idle(Box::new(move || {
            landed.store(true, Ordering::Release);
            wake.wake();
        }));
        let mut model = Self {
            shared_display_prefs: cx.follow::<shared_displays::SharedDisplayPrefs>(),
            automatic_display_prefs: cx.follow::<shared_displays::AutomaticDisplayPrefs>(),
            pending_change: cx.follow::<PendingDisplayChange>(),
            prefs: cx.follow::<DisplayPrefs>(),
            cx,
            of_device: true,
            brightness_control: control,
            outputs,
            outputs_read,
            ambient_sensor_available: false,
            brightness: 0.5,
            output_config,
            output_brightness,
            dimmed_by_tilo,
            pending_candidate: None,
            pending_device_move: None,
            awaiting_device_move: None,
            shared_move_revisions: None,
            device_move_reply: None,
            last_seen_transaction: None,
            host_state: None,
            host_probe: None,
            host_poll_deadline: None,
            last_snapshot_check: ui::time::now(),
            snapshot_stamp: settings_store::hardware::display_snapshot_path()
                .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
            following: std::rc::Weak::new(),
            settling: false,
            levels_landed,
            hardware_probe: Some(probe_rx),
            brightness_error,
            brightness_tx: Some(brightness_tx),
            brightness_result_rx: Some(result_rx),
            pending_brightness: None,
            notice: None,
            arrangement_drag: LocalState::new(
                super::display_arrangement::ArrangementGesture::default(),
            ),
            device_arrangement_drag: LocalState::new(
                super::display_arrangement::ArrangementGesture::default(),
            ),
            selected: LocalState::new(String::new()),
            selected_seen: 0,
            pointed: LocalState::new(None),
            pointed_seen: 0,
            last_flash: None,
            identify_window: None,
            sample: None,
            person_devices: settings_store::person_displays::PersonDisplays::load()
                .map_or_else(Vec::new, |view| view.devices),
            device_displays: settings_store::device_displays::DeviceDisplaySnapshot::load()
                .map_or_else(Vec::new, |snapshot| snapshot.displays),
            changed: false,
        };
        model.normalize_selection();
        model
    }

    /// A built-in panel at 62 %, with an ambient light sensor, and for the
    /// displays captures a desk monitor beside it; nothing it does reaches
    /// the device or its files.
    fn sample(cx: PaneCx) -> Self {
        let device_displays = cx
            .scene()
            .map_or_else(Vec::new, |scene| scene.device_displays.clone());
        let displays = cx.scene().and_then(|scene| scene.displays);
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let panel = DisplayOutput {
            label: "Built-in display".into(),
            connected: true,
            builtin: true,
            connector: "eDP-1".into(),
            connector_label: "eDP-1".into(),
            modes: vec![mode],
            current_mode: Some(mode),
            scale: 1.0,
            hdr: false,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            vrr: false,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profiles: Vec::new(),
            color_profile_labels: BTreeMap::new(),
            monitor: None,
        };
        let mut outputs = vec![panel];
        if displays.is_some() {
            outputs.push(sample_monitor());
        }
        let mut model = Self::detached(cx).with_outputs(outputs);
        model.person_devices = model
            .cx
            .scene()
            .map_or_else(Vec::new, |scene| scene.person_devices.clone());
        model.device_displays = device_displays;
        if let Some(sample) = displays {
            model.stage_sample(sample);
        }
        model
    }

    /// Set the displays capture's scene up as a person would have: the
    /// monitor to the right of the panel, then chosen, held mid-drag or
    /// identified as `sample` says.
    fn stage_sample(&mut self, sample: crate::scenarios::DisplaysSample) {
        use super::display_arrangement::{ArrangementGesture, placement::Placement};
        use crate::scenarios::DisplaysSample;
        self.output_config.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                ..OutputConfig::at_scale(1.0)
            },
        );
        self.output_config.outputs.insert(
            SAMPLE_MONITOR.into(),
            OutputConfig {
                x: 1920,
                y: -360,
                ..OutputConfig::at_scale(1.0)
            },
        );
        self.output_brightness.insert(SAMPLE_MONITOR.into(), 0.7);
        self.sample = Some(sample);
        match sample {
            DisplaysSample::Default => {}
            DisplaysSample::ChosenByKeyboard | DisplaysSample::PickedOnCanvas => {
                self.selected.set(SAMPLE_MONITOR.into());
            }
            DisplaysSample::Dragging => {
                // Held under the panel, its centre a few pixels off the
                // panel's: it snaps there, and the guides say so.
                self.selected.set(SAMPLE_MONITOR.into());
                self.arrangement_drag.set(ArrangementGesture::held(
                    SAMPLE_MONITOR,
                    Placement::new(-310, 1092, 2560, 1440),
                ));
            }
            DisplaysSample::Identifying => {
                let now = ui::time::now();
                self.identify_window = Some((
                    now,
                    now + Duration::from_millis(settings_store::display_identify::IDENTIFY_MS),
                ));
            }
        }
        self.selected_seen = self.selected.revision();
    }

    /// A model with no outputs whose brightness changes nothing; tests give
    /// it what they drive.
    fn detached(cx: PaneCx) -> Self {
        Self {
            shared_display_prefs: cx.follow::<shared_displays::SharedDisplayPrefs>(),
            automatic_display_prefs: cx.follow::<shared_displays::AutomaticDisplayPrefs>(),
            pending_change: cx.follow::<PendingDisplayChange>(),
            prefs: cx.follow::<DisplayPrefs>(),
            cx,
            of_device: false,
            brightness_control: Arc::new(NoBrightness),
            outputs: Vec::new(),
            outputs_read: true,
            ambient_sensor_available: true,
            brightness: 0.62,
            output_config: DisplayConfig::default(),
            output_brightness: BTreeMap::new(),
            dimmed_by_tilo: BTreeSet::new(),
            pending_candidate: None,
            pending_device_move: None,
            awaiting_device_move: None,
            shared_move_revisions: None,
            device_move_reply: None,
            last_seen_transaction: None,
            host_state: None,
            host_probe: None,
            host_poll_deadline: None,
            last_snapshot_check: ui::time::now(),
            snapshot_stamp: None,
            following: std::rc::Weak::new(),
            settling: false,
            levels_landed: Arc::new(AtomicBool::new(false)),
            hardware_probe: None,
            brightness_error: None,
            brightness_tx: None,
            brightness_result_rx: None,
            pending_brightness: None,
            notice: None,
            arrangement_drag: LocalState::new(
                super::display_arrangement::ArrangementGesture::default(),
            ),
            device_arrangement_drag: LocalState::new(
                super::display_arrangement::ArrangementGesture::default(),
            ),
            selected: LocalState::new(String::new()),
            selected_seen: 0,
            pointed: LocalState::new(None),
            pointed_seen: 0,
            last_flash: None,
            identify_window: None,
            sample: None,
            device_displays: Vec::new(),
            person_devices: Vec::new(),
            changed: false,
        }
    }

    #[must_use]
    fn with_outputs(mut self, outputs: Vec<DisplayOutput>) -> Self {
        self.outputs = outputs;
        self.normalize_selection();
        self
    }

    /// The selected display, by output key: the one chosen on the canvas
    /// while it is connected, else the primary display, else
    /// the built-in panel, else the first.
    fn selected_output(&self) -> Option<&DisplayOutput> {
        let connected = self.connected_outputs();
        let chosen = self.selected.with(Clone::clone);
        connected
            .iter()
            .find(|output| output.connector == chosen)
            .or_else(|| {
                connected.iter().find(|output| {
                    self.staged()
                        .outputs
                        .get(&output.connector)
                        .is_some_and(|config| config.primary && config.enabled)
                })
            })
            .or_else(|| connected.iter().find(|output| output.builtin))
            .or_else(|| connected.first())
            .copied()
    }

    /// Keep the shared selection on a connected display: one that went away
    /// hands it to [`Self::selected_output`]'s choice, quietly (nothing is
    /// flashed for a choice the person did not make).
    fn normalize_selection(&mut self) {
        let effective = self
            .selected_output()
            .map(|output| output.connector.clone())
            .unwrap_or_default();
        if self.selected.with(|selected| *selected != effective) {
            self.selected.set(effective);
            self.selected_seen = self.selected.revision();
        }
    }

    /// Take in a choice made on the canvas, and the
    /// display the pointer came to rest on: the page shows the chosen
    /// display's settings, and each such display flashes its number on
    /// itself, so the person always knows which screen is which.
    fn poll_selection(&mut self) {
        let revision = self.selected.revision();
        if revision != self.selected_seen {
            self.selected_seen = revision;
            self.changed = true;
            let chosen = self.selected.with(Clone::clone);
            if !chosen.is_empty() {
                self.flash(&chosen);
            }
        }
        let revision = self.pointed.revision();
        if revision != self.pointed_seen {
            self.pointed_seen = revision;
            if let Some(pointed) = self.pointed.with(Clone::clone) {
                self.flash(&pointed);
            }
        }
        self.normalize_selection();
    }

    /// Identify shows each display's number from now on: the canvas
    /// tiles show the same numbers in the accent while it does.
    fn identify_shown(&mut self) {
        let now = ui::time::now();
        self.identify_window = Some((
            now,
            now + Duration::from_millis(settings_store::display_identify::IDENTIFY_MS),
        ));
        self.changed = true;
    }

    /// Briefly show `display`'s number on that display alone.
    fn flash(&mut self, display: &str) {
        if !self
            .connected_outputs()
            .iter()
            .any(|output| output.connector == display)
        {
            return;
        }
        self.last_flash = Some(display.to_owned());
        if !self.of_device {
            return;
        }
        let command = settings_store::display_control::DisplayCommand::IdentifyDisplay {
            display: display.to_owned(),
        };
        // A glance, not a transaction: what the host answers changes
        // nothing here, and a refused flash costs nothing.
        let _ = crate::device_read::thread(
            "settings-display-flash",
            Box::new(move || {
                let _ = request_host_display(&command, ui::collaboration::DisplayOrigin::LocalHost);
            }),
        );
    }

    /// The outputs the arrangement card lists as rows: only the connected
    /// ones. Disconnected physical ports are unused hardware, not displays —
    /// enumerating each one (a laptop's GPU can expose a dozen idle DP
    /// connectors) is noise, so they are summarised in a caption instead.
    fn connected_outputs(&self) -> Vec<&DisplayOutput> {
        self.outputs.iter().filter(|o| o.connected).collect()
    }

    /// Whether the staged change can apply at once on this device, without
    /// the 15-second try: nothing in it can leave a screen dark
    /// ([`DisplayConfig::needs_confirmation_from`]), and no other change is
    /// being tried.
    fn staged_applies_at_once(&self) -> bool {
        let Some(candidate) = &self.pending_candidate else {
            return false;
        };
        let trying = self.transaction().is_some_and(|transaction| {
            matches!(
                transaction.state,
                DisplayTransactionState::Requested
                    | DisplayTransactionState::AwaitingConfirmation
                    | DisplayTransactionState::ConfirmRequested
                    | DisplayTransactionState::RevertRequested
            )
        });
        let mut committed = self.output_config.clone();
        for output in self.connected_outputs() {
            committed
                .outputs
                .entry(output.connector.clone())
                .or_insert_with(|| output.unconfigured());
        }
        !trying && !candidate.needs_confirmation_from(&committed)
    }

    /// The arrangement as it stands: the staged one, else the committed one.
    fn staged(&self) -> &DisplayConfig {
        self.pending_candidate
            .as_ref()
            .unwrap_or(&self.output_config)
    }

    /// The display change being tried, or the last one: as the settings
    /// document Settings reads over its channel shows it, unless the host's
    /// last answer names a newer one. Observed output timing stays separate
    /// from a requested candidate.
    fn transaction(&self) -> Option<DisplayTransaction> {
        let shown = self
            .of_device
            .then(|| self.pending_change.get().transaction())
            .flatten();
        let answered = self
            .host_state
            .as_ref()
            .and_then(|state| state.transaction.clone());
        match (shown, answered) {
            (Some(shown), Some(answered)) if answered.id > shown.id => Some(answered),
            (Some(shown), _) => Some(shown),
            (None, answered) => answered,
        }
    }

    fn host_command(
        &self,
        effect: &DisplayEffect,
    ) -> Result<settings_store::display_control::DisplayCommand, String> {
        use settings_store::display_control::DisplayCommand;
        match effect {
            DisplayEffect::InspectDisplayConfiguration => Ok(DisplayCommand::Inspect),
            DisplayEffect::IdentifyDisplays => Ok(DisplayCommand::Identify),
            DisplayEffect::ApplyDisplayChanges => Ok(DisplayCommand::Apply {
                observed_set: self.display_set(),
                candidate: self
                    .pending_candidate
                    .clone()
                    .ok_or("There are no staged display changes to apply.")?,
            }),
            DisplayEffect::KeepDisplayChanges => Ok(DisplayCommand::Keep {
                transaction: self
                    .transaction()
                    .ok_or("There is no display preview to keep.")?
                    .id,
            }),
            DisplayEffect::RevertDisplayChanges => Ok(DisplayCommand::Revert {
                transaction: self
                    .transaction()
                    .ok_or("There is no display preview to revert.")?
                    .id,
            }),
            _ => Err("This action is not a display transaction request.".into()),
        }
    }

    fn accept_host_reply(&mut self, reply: settings_store::display_control::DisplayReply) {
        let pending = reply.transaction.as_ref().is_some_and(awaits_answer);
        self.settling = pending;
        let same = self
            .host_state
            .as_ref()
            .and_then(|state| state.transaction.as_ref())
            .map(|transaction| transaction.id)
            == reply.transaction.as_ref().map(|transaction| transaction.id);
        if pending && (!same || self.host_poll_deadline.is_none()) {
            self.host_poll_deadline = Some(ui::time::now() + Duration::from_secs(30));
        } else if !pending {
            self.host_poll_deadline = None;
        }
        if let Some(transaction) = &reply.transaction {
            self.settle(transaction);
        }
        if reply
            .identify
            .as_ref()
            .is_some_and(|request| request.display.is_none())
        {
            self.identify_shown();
        }
        self.outputs = outputs_from_snapshot(&reply.observed);
        self.outputs_read = true;
        self.output_config = reply.committed.clone();
        self.host_state = Some(reply);
        self.changed = true;
    }

    fn inspection(&self) -> serde_json::Value {
        serde_json::json!({
            "observed_outputs": self.connected_outputs().into_iter().map(|output| {
                serde_json::json!({
                    "display": output.connector,
                    "label": output_title(output),
                    "current_mode": output.current_mode,
                    "supported_modes": output.modes,
                    "scale": output.scale,
                    "hdr_available": output.hdr,
                    "vrr_available": output.vrr,
                })
            }).collect::<Vec<_>>(),
            "device_displays": self.device_displays,
            "shared_displays": self.shared_display_inspection(),
            "saved_configuration": self.output_config,
            "staged_configuration": self.staged(),
            "has_staged_changes": self.pending_candidate.is_some(),
            "transaction": self.transaction(),
            "notice": self.notice,
        })
    }

    /// Each control and the semantic catalogue use this same fixed-output
    /// resource. The caller supplies a value, never another output identity.
    fn output_row(&self, output: &DisplayOutput) -> LiveRow {
        let display = &output.connector;
        let mut row = LiveRow::new(
            format!("settings.display-output:{display}"),
            "settings.display-output",
            output_title(output),
        );
        let target = || display.clone();
        let operations = [
            (
                DisplayEffect::UseDisplay {
                    display: target(),
                    on: false,
                },
                vec!["on"],
            ),
            (
                DisplayEffect::SetPrimaryDisplay { display: target() },
                vec![],
            ),
            (
                DisplayEffect::SetOutputScale {
                    display: target(),
                    scale: String::new(),
                },
                vec!["scale"],
            ),
            (
                DisplayEffect::PlaceDisplay {
                    display: target(),
                    position: String::new(),
                },
                vec!["position"],
            ),
            (
                DisplayEffect::MoveDisplay {
                    display: target(),
                    x: 0,
                    y: 0,
                },
                vec!["x", "y"],
            ),
            (
                DisplayEffect::MirrorDisplay {
                    display: target(),
                    mirror: String::new(),
                },
                vec!["mirror"],
            ),
            (
                DisplayEffect::RotateDisplay {
                    display: target(),
                    rotation: String::new(),
                },
                vec!["rotation"],
            ),
        ];
        for (effect, asking) in operations {
            row = row.operation(self.output_operation(output, effect, &asking));
        }
        if !output.modes.is_empty() {
            row = row.operation(self.output_operation(
                output,
                DisplayEffect::SetDisplayMode {
                    display: target(),
                    mode: String::new(),
                },
                &["mode"],
            ));
        }
        self.output_capability_row(output, row)
    }

    fn output_operation(
        &self,
        output: &DisplayOutput,
        effect: DisplayEffect,
        asking: &[&str],
    ) -> ui::tilo_actions::ActionSpec {
        let choices = match &effect {
            DisplayEffect::SetDisplayMode { .. } => output
                .modes
                .iter()
                .copied()
                .map(OutputMode::label)
                .collect(),
            DisplayEffect::SetOutputScale { .. } => DISPLAY_SCALES
                .iter()
                .map(|(label, _)| (*label).to_owned())
                .collect(),
            DisplayEffect::RotateDisplay { .. } => transform_options(),
            DisplayEffect::PlaceDisplay { .. } => self.relative_choices(output, false),
            DisplayEffect::MirrorDisplay { .. } => self.relative_choices(output, true),
            _ => Vec::new(),
        };
        let effect = SettingsEffect::from(effect);
        let mut params = effect.bound_asking(asking).params().to_vec();
        if let Some(param) = params.first_mut() {
            param.choices = choices.into_iter().map(Into::into).collect();
        }
        effect.bound_asking_params(params)
    }

    fn relative_choices(&self, output: &DisplayOutput, mirror: bool) -> Vec<String> {
        let mut choices = vec![
            if mirror {
                "Extend workspace"
            } else {
                "Automatic"
            }
            .to_owned(),
        ];
        for target in self
            .connected_outputs()
            .into_iter()
            .filter(|target| target.connector != output.connector)
        {
            if mirror {
                choices.push(format!("Mirror {}", target.label));
            } else {
                choices.extend(
                    ["Left", "Right", "Above", "Below"]
                        .map(|side| format!("{side} of {}", target.label)),
                );
            }
        }
        choices
    }

    fn output_capability_row(&self, output: &DisplayOutput, mut row: LiveRow) -> LiveRow {
        let display = &output.connector;
        let target = || display.clone();
        let offer = |effect: DisplayEffect, asking: &[&str]| {
            SettingsEffect::from(effect).bound_asking(asking)
        };
        if self.output_brightness.contains_key(display) {
            row = row.operation(offer(
                DisplayEffect::SetMonitorBrightness {
                    display: target(),
                    brightness: 0.0,
                },
                &["brightness"],
            ));
        }
        if output.hdr {
            row = row
                .operation(offer(
                    DisplayEffect::SetHdr {
                        display: target(),
                        on: false,
                    },
                    &["on"],
                ))
                .operation(offer(
                    DisplayEffect::SetSdrBrightness {
                        display: target(),
                        brightness: 0.0,
                    },
                    &["brightness"],
                ));
        }
        if output.vrr {
            row = row.operation(offer(
                DisplayEffect::SetVrr {
                    display: target(),
                    on: false,
                },
                &["on"],
            ));
        }
        if !output.color_profiles.is_empty() {
            row = row.operation(offer(
                DisplayEffect::SetColorProfile {
                    display: target(),
                    profile: String::new(),
                },
                &["profile"],
            ));
        }
        row
    }

    fn output_binding<I: 'static>(
        &self,
        display: &str,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> Option<ui::tilo_actions::ActionBinding<I>> {
        let output = self
            .connected_outputs()
            .into_iter()
            .find(|output| output.connector == display)?;
        self.cx
            .row_binding(&self.output_row(output).into_resource(), effect)
    }

    fn bind_output<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        display: &str,
        control: C,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        match self.output_binding(display, effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control`'s widget id for `field` of `display`.
    fn id(display: &str, field: &str) -> String {
        output_key(display, field)
    }

    /// A toggle for `field` of `display`, invoking `set` with it.
    fn toggle(
        &self,
        display: &str,
        field: &str,
        on: bool,
        set: ActionRef<SettingsEffect, (String, bool)>,
    ) -> Toggle {
        let target = display.to_owned();
        self.bind_output(
            display,
            Toggle::from_state(&LocalState::new(on)).with_id(&Self::id(display, field)),
            set.map(move |on: bool| (target.clone(), on)),
        )
    }

    /// A slider for `field` of `display`, invoking `set` with it.
    fn slider(
        &self,
        display: &str,
        field: &str,
        value: f64,
        set: ActionRef<SettingsEffect, (String, f64)>,
    ) -> Slider {
        let target = display.to_owned();
        self.bind_output(
            display,
            Slider::from_state(&LocalState::new(value))
                .shows_value()
                .with_id(&Self::id(display, field)),
            set.map(move |value: f64| (target.clone(), value)),
        )
    }

    /// A chooser for `field` of `display`, invoking `set` with the choice.
    fn choice(
        &self,
        display: &str,
        field: &str,
        choices: Vec<String>,
        current: &str,
        set: ActionRef<SettingsEffect, (String, String)>,
    ) -> Dropdown {
        let target = display.to_owned();
        self.bind_output(
            display,
            Dropdown::from_state(choices, &LocalState::new(current.to_owned()))
                .with_id(&Self::id(display, field))
                .with_placeholder(current),
            set.map(move |choice: String| (target.clone(), choice)),
        )
    }

    /// Carry out `effect`; what it reported.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect; the staged ones share `stage`"
    )]
    fn apply(&mut self, effect: &DisplayEffect) -> Result<String, String> {
        if effect == &DisplayEffect::InspectDisplayConfiguration {
            return Err("Display inspection is read-only.".into());
        }
        self.changed = true;
        let reported = match effect {
            DisplayEffect::SetAutomaticDisplayInput { on } => self.set_automatic_display_input(*on),
            DisplayEffect::IncludeSharedDisplay { .. }
            | DisplayEffect::OrderSharedDisplay { .. } => self.edit_shared_display(effect),
            DisplayEffect::InspectSharedDisplays => Err(t("display-shared-read-only")),
            DisplayEffect::MoveDeviceDisplay {
                display,
                revision,
                x,
                y,
            } => {
                if self.device_move_reply.is_some() || self.awaiting_device_move.is_some() {
                    return Err(t("display-device-move-pending"));
                }
                let screen = self
                    .device_displays
                    .iter()
                    .find(|screen| {
                        screen.key() == *display
                            && screen.revision == *revision
                            && DisplayModel::device_movable(screen)
                    })
                    .ok_or_else(|| t("display-device-move-unavailable"))?
                    .clone();
                self.shared_move_revisions = self.capture_shared_move_revisions(&screen.device);
                self.pending_device_move =
                    Some(settings_store::device_displays::DeviceDisplayMove {
                        screen,
                        x: *x,
                        y: *y,
                    });
                self.awaiting_device_move = self
                    .pending_device_move
                    .clone()
                    .map(|request| (request, ui::time::now()));
                self.device_move_reply = Some(Rc::new(std::cell::RefCell::new(
                    ui::widget::DeviceDisplayMoveReply::default(),
                )));
                Ok(t("display-device-move-pending"))
            }
            DisplayEffect::InspectDisplayConfiguration => {
                Err("Display inspection is read-only.".into())
            }
            DisplayEffect::SetBrightness { brightness } => self.set_brightness(*brightness),
            DisplayEffect::SetAutomaticBrightness { on } => {
                if *on && !self.ambient_sensor_available {
                    return Err(
                        "Automatic brightness is unavailable because this device has no ambient-light sensor."
                            .into(),
                    );
                }
                let on = *on;
                self.save(|prefs| prefs.auto_brightness = on)?;
                Ok(if on {
                    "Automatic brightness enabled.".into()
                } else {
                    "Automatic brightness disabled.".into()
                })
            }
            DisplayEffect::SetMonitorBrightness {
                display,
                brightness,
            } => {
                let level = (*brightness as f32).clamp(0.0, 1.0);
                let label = self
                    .outputs
                    .iter()
                    .find(|output| &output.connector == display)
                    .filter(|_| self.output_brightness.contains_key(display))
                    .map(|output| output.connector_label.clone())
                    .ok_or_else(|| "that monitor's brightness cannot be set".to_string())?;
                self.brightness_control
                    .set_display_brightness(&label, level * 100.0)
                    .map_err(|error| {
                        warn!(target: "tilo::settings", op = "set_display_brightness", %error, "display brightness failed");
                        format!("Brightness could not be changed: {error}")
                    })?;
                self.output_brightness.insert(display.clone(), level);
                Ok(format!("Brightness set to {}%.", (level * 100.0).round()))
            }
            DisplayEffect::UseDisplay { display, on } => self.stage(display, |cfg, _, _| {
                cfg.enabled = *on;
                if !on {
                    cfg.primary = false;
                }
                Ok(())
            }),
            DisplayEffect::SetPrimaryDisplay { display } => self.stage(display, |cfg, _, _| {
                cfg.primary = true;
                Ok(())
            }),
            DisplayEffect::SetOutputScale { display, scale } => {
                let factor =
                    scale_factor_of(scale).ok_or_else(|| format!("`{scale}` is not a scale"))?;
                self.stage(display, |cfg, _, _| {
                    cfg.scale = factor;
                    Ok(())
                })
            }
            DisplayEffect::PlaceDisplay { display, position } => self
                .stage(display, |cfg, next, connected| {
                    place_beside(display, position, cfg, next, connected)
                }),
            DisplayEffect::MoveDisplay { display, x, y } => self.stage(display, |cfg, _, _| {
                cfg.x = *x;
                cfg.y = *y;
                Ok(())
            }),
            DisplayEffect::MirrorDisplay { display, mirror } => {
                self.stage(display, |cfg, _, connected| {
                    cfg.mirror_of = if mirror == "Extend workspace" {
                        None
                    } else {
                        Some(
                            connected
                                .iter()
                                .find(|candidate| *mirror == format!("Mirror {}", candidate.label))
                                .map(|candidate| candidate.connector.clone())
                                .ok_or_else(|| format!("`{mirror}` is not a display to mirror"))?,
                        )
                    };
                    Ok(())
                })
            }
            DisplayEffect::SetDisplayMode { display, mode } => {
                self.stage(display, |cfg, _, connected| {
                    cfg.mode = Some(
                        output_of(connected, display)?
                            .modes
                            .iter()
                            .copied()
                            .find(|candidate| candidate.label() == *mode)
                            .ok_or_else(|| format!("`{mode}` is not a mode of this display"))?,
                    );
                    Ok(())
                })
            }
            DisplayEffect::RotateDisplay { display, rotation } => {
                let transform = transform_from_label(rotation)
                    .ok_or_else(|| format!("`{rotation}` is not a rotation"))?;
                self.stage(display, |cfg, _, _| {
                    cfg.transform = transform;
                    Ok(())
                })
            }
            DisplayEffect::SetHdr { display, on } => self.stage(display, |cfg, _, connected| {
                if !output_of(connected, display)?.hdr {
                    return Err("this display does not offer HDR".into());
                }
                cfg.hdr = *on;
                Ok(())
            }),
            DisplayEffect::SetSdrBrightness {
                display,
                brightness,
            } => self.stage(display, |cfg, _, connected| {
                if !output_of(connected, display)?.hdr {
                    return Err("this display does not offer HDR".into());
                }
                cfg.sdr_brightness_nits = sdr_brightness_nits(*brightness);
                Ok(())
            }),
            DisplayEffect::SetVrr { display, on } => self.stage(display, |cfg, _, connected| {
                if !output_of(connected, display)?.vrr {
                    return Err("this display does not offer a variable refresh rate".into());
                }
                cfg.vrr = *on;
                Ok(())
            }),
            DisplayEffect::SetColorProfile { display, profile } => {
                self.stage(display, |cfg, _, connected| {
                    cfg.color_profile = if profile == "System default" {
                        None
                    } else {
                        let output = output_of(connected, display)?;
                        Some(
                            output
                                .color_profile_labels
                                .iter()
                                .find_map(|(id, label)| {
                                    (*profile == format!("{label} ({id})") || profile == id)
                                        .then(|| id.clone())
                                })
                                .ok_or_else(|| format!("`{profile}` is not a colour profile"))?,
                        )
                    };
                    Ok(())
                })
            }
            DisplayEffect::IdentifyDisplays => {
                settings_store::display_identify::IdentifyRequest::request()
                    .map(|_| {
                        self.identify_shown();
                        "Each display shows its number.".into()
                    })
                    .map_err(|error| format!("Displays could not be identified: {error}"))
            }
            DisplayEffect::ApplyDisplayChanges => {
                let candidate = self
                    .pending_candidate
                    .clone()
                    .ok_or_else(|| "There are no staged display changes to apply.".to_string())?;
                DisplayTransaction::request(self.display_set(), candidate)
                    .map(|_| "Previewing the display changes.".into())
                    .map_err(|error| format!("Display configuration could not be staged: {error}"))
            }
            DisplayEffect::KeepDisplayChanges => {
                let mut transaction = DisplayTransaction::load()
                    .ok_or_else(|| "There is no display transaction to keep.".to_string())?;
                transaction
                    .request_confirmation()
                    .map(|()| "Keeping the display changes.".into())
                    .map_err(|error| format!("Display configuration could not be kept: {error}"))
            }
            DisplayEffect::RevertDisplayChanges => {
                let mut transaction = DisplayTransaction::load()
                    .ok_or_else(|| "There is no display transaction to revert.".to_string())?;
                transaction
                    .request_revert()
                    .map(|()| "Reverting the display changes.".into())
                    .map_err(|error| {
                        format!("Display configuration could not be reverted: {error}")
                    })
            }
            DisplayEffect::ForgetDisplayTool { tool } => {
                settings_store::display_tools::TrustedDisplayTools::forget(tool)
                    .map(|()| t("display-tools-forgotten"))
                    .map_err(|error| format!("The display tool was not forgotten: {error}"))
            }
        };
        self.notice = Some(match &reported {
            Ok(message) => message.clone(),
            Err(error) => error.clone(),
        })
        .filter(|notice| !notice.is_empty());
        reported
    }

    /// Set the panel's brightness: queued for the worker, or set now.
    fn set_brightness(&mut self, brightness: f64) -> Result<String, String> {
        let level = (brightness as f32).clamp(0.0, 1.0);
        let percent = level * 100.0;
        if let Some(sender) = &self.brightness_tx {
            sender.send(percent).map_err(|_| {
                "Brightness worker is unavailable; the display was not changed.".to_string()
            })?;
            self.pending_brightness = Some(level);
            return Ok(format!("Setting the brightness to {}%.", percent.round()));
        }
        self.brightness_control
            .set_brightness(percent)
            .map_err(|error| format!("Brightness could not be changed: {error}"))?;
        self.brightness = level;
        Ok(format!("Brightness set to {}%.", percent.round()))
    }

    /// Change the person's display settings.
    fn save(&mut self, change: impl FnOnce(&mut DisplayPrefs)) -> Result<(), String> {
        self.prefs.settings().update(change).map_err(|error| {
            warn!(target: "tilo::settings", op = "display_prefs_save", %error, "failed to persist display prefs");
            format!("Display preference could not be saved: {error}")
        })?;
        self.prefs.refresh();
        Ok(())
    }

    /// Stage a change of `display`: `edit` changes its entry of the staged
    /// arrangement, which is then made consistent (one primary display,
    /// mirrored pairs on a shared mode) and checked against what the
    /// displays can do. Nothing reaches the compositor until it is applied.
    fn stage(
        &mut self,
        display: &str,
        edit: impl FnOnce(&mut OutputConfig, &DisplayConfig, &[DisplayOutput]) -> Result<(), String>,
    ) -> Result<String, String> {
        let connected: Vec<DisplayOutput> = self.connected_outputs().into_iter().cloned().collect();
        let output = output_of(&connected, display)?;
        let mut next = self.staged().clone();
        for (index, candidate) in connected.iter().enumerate() {
            next.outputs
                .entry(candidate.connector.clone())
                .or_insert_with(|| OutputConfig {
                    primary: index == 0,
                    ..candidate.unconfigured()
                });
        }
        let mut cfg = next
            .outputs
            .get(&output.connector)
            .cloned()
            .unwrap_or_else(|| output.unconfigured());
        let was_primary = cfg.primary;
        edit(&mut cfg, &next, &connected)?;
        let preferred = (cfg.primary && !was_primary).then(|| display.to_owned());
        next.outputs.insert(output.connector.clone(), cfg);
        choose_primary(&mut next, &connected, preferred.as_deref());
        mirror_on_shared_modes(&mut next, &connected);
        let descriptors: Vec<_> = connected.iter().map(Self::output_descriptor).collect();
        next.validate_for(&descriptors).map_err(|error| {
            warn!(target: "tilo::settings", ?error, "rejected invalid display configuration");
            format!("That display configuration is not possible: {error:?}")
        })?;
        self.pending_candidate = Some(next);
        Ok(STAGED.into())
    }

    /// Reconstruct the compositor's descriptor contract from a Settings
    /// output. `label` is presentation-only and may include `· <bus-id>`;
    /// validation must retain the raw DRM connector label separately.
    fn output_descriptor(output: &DisplayOutput) -> OutputDescriptor {
        OutputDescriptor {
            connector: output.connector.clone(),
            connector_label: output.connector_label.clone(),
            modes: output.modes.clone(),
            hdr: output.hdr,
            vrr: output.vrr,
            color_profiles: output.color_profiles.clone(),
            monitor: output.monitor.clone(),
        }
    }

    /// The connected monitors, whose saved arrangement this pane edits.
    fn display_set(&self) -> DisplaySet {
        Self::display_set_of(&self.outputs)
    }

    fn display_set_of(outputs: &[DisplayOutput]) -> DisplaySet {
        let descriptors: Vec<_> = outputs
            .iter()
            .filter(|output| output.connected)
            .map(Self::output_descriptor)
            .collect();
        DisplaySet::of_descriptors(&descriptors)
    }

    /// Take in the brightness probe and the brightness worker's answers.
    fn poll_background(&mut self) {
        if let Some(receiver) = &self.hardware_probe {
            match receiver.try_recv() {
                Ok((brightness, ambient)) => {
                    self.ambient_sensor_available = ambient;
                    match brightness {
                        Ok(brightness) => {
                            self.brightness = brightness;
                            self.brightness_error = None;
                        }
                        Err(error) => self.brightness_error = Some(error),
                    }
                    self.hardware_probe = None;
                    self.changed = true;
                }
                Err(crate::channel::TryRecvError::Disconnected) => self.hardware_probe = None,
                Err(crate::channel::TryRecvError::Empty) => {}
            }
        }
        if let Some(result) = self
            .brightness_result_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
            && let Some(requested) = self.pending_brightness.take()
        {
            match result {
                Ok(applied) => self.brightness = applied,
                Err(error) => {
                    warn!(target: "tilo::settings", op = "set_brightness", %error, requested, "display worker dispatch failed");
                    self.notice = Some(format!("Brightness could not be changed: {error}"));
                }
            }
            self.changed = true;
        }
    }

    /// Take in the compositor's answer to an inspection as soon as it
    /// lands: its thread wakes the window.
    fn take_host_reply(&mut self) {
        let Some(receiver) = &self.host_probe else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(reply)) => {
                self.host_probe = None;
                self.accept_host_reply(reply);
            }
            Ok(Err(error)) => {
                self.host_probe = None;
                self.notice = Some(error);
                self.changed = true;
            }
            Err(crate::channel::TryRecvError::Disconnected) => {
                self.host_probe = None;
            }
            Err(crate::channel::TryRecvError::Empty) => {}
        }
    }

    /// Whether to read the compositor's snapshots again. A shown pane
    /// follows them: they are read when their watch says one (or the
    /// settings file, which holds a transaction) changed, when the monitor
    /// scan landed, and each second only while a change awaits an answer.
    /// Otherwise, or when they cannot all be watched, at most once a second.
    fn should_look(&self) -> bool {
        let due = self.last_snapshot_check.elapsed() >= Duration::from_secs(1);
        let Some(watched) = self
            .following
            .upgrade()
            .filter(|watched| watched.is_complete())
        else {
            return due;
        };
        // Take both signals, so one change is read once.
        let changed = watched.take();
        let landed = self.levels_landed.swap(false, Ordering::AcqRel);
        changed || landed || (self.settling && due)
    }

    /// Re-read the compositor's output snapshot when it moved
    /// ([`Self::should_look`]), and take in a settled display transaction.
    fn poll_device(&mut self) {
        if !self.of_device {
            return;
        }
        self.take_host_reply();
        if !self.should_look() {
            return;
        }
        self.last_snapshot_check = ui::time::now();
        if self.host_probe.is_none()
            && self
                .host_poll_deadline
                .is_some_and(|deadline| ui::time::now() < deadline)
            && self
                .host_state
                .as_ref()
                .and_then(|state| state.transaction.as_ref())
                .is_some_and(awaits_answer)
        {
            let (sender, receiver) = crate::channel::sync_channel(1);
            self.host_probe = Some(receiver);
            let wake = ui::WakeHandle::new();
            let _ = crate::device_read::thread(
                "settings-display-host-probe",
                Box::new(move || {
                    let _ = sender.send(request_host_display(
                        &settings_store::display_control::DisplayCommand::Inspect,
                        ui::collaboration::DisplayOrigin::LocalHost,
                    ));
                    wake.wake();
                }),
            );
        }
        let devices = settings_store::device_displays::DeviceDisplaySnapshot::load()
            .map_or_else(Vec::new, |snapshot| snapshot.displays);
        if devices != self.device_displays {
            self.device_displays = devices;
            self.changed = true;
        }
        let person_devices = settings_store::person_displays::PersonDisplays::load()
            .map_or_else(Vec::new, |view| view.devices);
        if self.person_devices != person_devices {
            self.person_devices = person_devices;
            self.changed = true;
        }
        let stamp = settings_store::hardware::display_snapshot_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        if stamp != self.snapshot_stamp {
            self.snapshot_stamp = stamp;
            let read = read_outputs();
            self.outputs_read = read.is_some();
            self.outputs = read.unwrap_or_default();
            self.output_config = self.host_state.as_ref().map_or_else(
                || DisplayConfig::load_for(&self.display_set()),
                |state| state.committed.clone(),
            );
            self.changed = true;
        }
        // Monitor probing finishes in the background; a monitor that turns
        // out to speak DDC/CI gains its slider once the probe lands.
        let controllable = read_output_brightness(&self.outputs);
        let dimmed_by_tilo = read_dimmed_by_tilo(&self.outputs);
        if controllable.keys().ne(self.output_brightness.keys())
            || dimmed_by_tilo != self.dimmed_by_tilo
        {
            self.output_brightness = controllable;
            self.dimmed_by_tilo = dimmed_by_tilo;
            self.changed = true;
        }
        let transaction = self.transaction();
        self.settling = transaction.as_ref().is_some_and(awaits_answer)
            && (self.host_state.is_none()
                || self
                    .host_poll_deadline
                    .is_some_and(|deadline| ui::time::now() < deadline));
        if let Some(transaction) = transaction {
            self.settle(&transaction);
        }
    }

    /// Once the compositor settled `transaction` (kept, reverted or
    /// rejected), show the committed arrangement again.
    fn settle(&mut self, transaction: &DisplayTransaction) {
        let marker = (transaction.id, transaction.state);
        if self.last_seen_transaction == Some(marker) {
            return;
        }
        self.last_seen_transaction = Some(marker);
        self.changed = true;
        // The transaction's `message` is the compositor's diagnostic, in its
        // terms and in English: the inspection carries it, the page says the
        // outcome in the person's words.
        let notice = match transaction.state {
            DisplayTransactionState::Confirmed => t("display-changes-kept"),
            DisplayTransactionState::Reverted => t("display-changes-reverted"),
            DisplayTransactionState::Rejected => t("display-rejected-caption"),
            _ => return,
        };
        self.output_config = self.host_state.as_ref().map_or_else(
            || DisplayConfig::load_for(&self.display_set()),
            |state| state.committed.clone(),
        );
        self.pending_candidate = None;
        self.notice = Some(notice);
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let connected = self.connected_outputs();
        // What is being tried, or waits to be, leads: its countdown must be
        // seen. Then the arrangement, which is the selection, and below it
        // the one selected display's settings: never a list of every
        // display, and no brightness outside the selected display's. Then
        // the comforts that are not one display's, and the displays of
        // other devices.
        let mut cards: Vec<SettingsSection> = self.build_safety_card().into_iter().collect();
        cards.push(self.build_arrangement_card(&connected));
        if let Some(notice) = &self.notice {
            cards.push(section(&t("display-card-title"), rows(vec![note(notice)])));
        }
        if let Some(output) = self.selected_output() {
            cards.push(self.build_output_card(output, &connected));
        }
        cards.push(self.build_night_light_card());
        if let Some(shared) = self.build_shared_displays_card() {
            cards.push(shared);
        }
        if let Some(devices) = self.build_device_arrangement_card() {
            cards.push(devices);
        }
        if let Some(tools) = self.build_trusted_tools_card() {
            cards.push(tools);
        }
        crate::widgets::page(cards)
    }

    /// Certified peer connectors at their observed shared logical positions.
    /// Observation alone never makes a screen movable or grants input access.
    fn build_device_arrangement_card(&self) -> Option<SettingsSection> {
        use super::display_arrangement::{
            ArrangedDisplay, DisplayArrangement, SharedDisplayState, placement::Placement,
        };
        if !self.device_displays.iter().any(|display| !display.local) {
            return None;
        }
        let layout = self.shared_layout();
        let mut unknown_number = layout.displays.len().saturating_add(layout.inactive.len());
        let first_local = self
            .device_displays
            .iter()
            .position(|display| display.local);
        let displays = self
            .device_displays
            .iter()
            .enumerate()
            .filter_map(|(index, display)| {
                let resolved = display.output_id.as_deref().and_then(|output| {
                    layout
                        .listed()
                        .find(|resolved| resolved.is(&display.device, output))
                });
                Some(ArrangedDisplay {
                    connector: display.key(),
                    movement_group: Some(display.device.clone()),
                    shared: resolved.map(|resolved| SharedDisplayState {
                        device: resolved.device.clone(),
                        output: resolved.output.clone(),
                        online: resolved.online,
                        included: resolved.enabled,
                    }),
                    label: t_args(
                        "display-device-screen",
                        &[
                            ("device", display.device_label.clone().into()),
                            ("screen", display.label.clone().into()),
                            (
                                "status",
                                t(if display.online {
                                    "display-device-online"
                                } else {
                                    "display-device-offline"
                                })
                                .into(),
                            ),
                        ],
                    ),
                    number: resolved.map_or_else(
                        || {
                            unknown_number = unknown_number.saturating_add(1);
                            unknown_number
                        },
                        |resolved| resolved.order.saturating_add(1),
                    ),
                    placement: Placement::new(
                        display.x,
                        display.y,
                        i32::try_from(display.width).ok()?,
                        i32::try_from(display.height).ok()?,
                    ),
                    primary: first_local == Some(index),
                    movable: DisplayModel::device_movable(display),
                })
            })
            .collect();
        let moves = self
            .device_displays
            .iter()
            .filter(|display| DisplayModel::device_movable(display))
            .filter_map(|display| {
                let key = display.key();
                let target = key.clone();
                let revision = display.revision;
                self.cx
                    .row_binding(
                        &DisplayModel::device_output_row(display).into_resource(),
                        effect(DisplayEffect::MOVE_DEVICE_DISPLAY)
                            .map(move |(x, y): (i32, i32)| (target.clone(), revision, x, y)),
                    )
                    .map(|binding| (key, binding))
            })
            .collect();
        Some(section(
            &t("display-devices-title"),
            rows(vec![
                block(Box::new(DisplayArrangement::new(
                    displays,
                    moves,
                    self.device_arrangement_drag.clone(),
                ))),
                note(&t("display-devices-arrange")),
            ]),
        ))
    }

    /// The arrangement card: the canvas of the connected displays, which is
    /// also where a display is chosen (by pointer, touch, keyboard or screen
    /// reader). Identify sits in the card's header. See [`Self::connected_outputs`]
    /// for why disconnected ports aren't enumerated.
    fn build_arrangement_card(&self, connected: &[&DisplayOutput]) -> SettingsSection {
        if connected.is_empty() {
            return section(
                &t("display-card-title"),
                rows(vec![note(&t(if self.outputs_read {
                    "display-none"
                } else {
                    "display-unreadable"
                }))]),
            );
        }
        let disconnected = self.outputs.len() - connected.len();
        let canvas = self.arrangement_canvas(connected);
        let identify = self.cx.bind_invocation(
            ui::widget::Button::new("settings.display.identify", &t("display-identify"))
                .role(ui::widget::ButtonRole::Secondary),
            DisplayEffect::IdentifyDisplays,
        );
        // One honest line about the idle ports rather than a row each, under
        // the heading, saying what the canvas is for.
        let caption = if connected.len() > 1 {
            t("display-caption-switch")
        } else if disconnected == 0 {
            t("display-caption-second")
        } else {
            t("display-caption-port")
        };
        crate::widgets::section_with(
            &t("display-card-title"),
            Box::new(identify),
            rows(vec![note(&caption), block(Box::new(canvas))]),
        )
    }

    /// The arrangement canvas over `connected`: it holds the page's one
    /// display selection and tells the model which display the pointer is
    /// over.
    fn arrangement_canvas(
        &self,
        connected: &[&DisplayOutput],
    ) -> super::display_arrangement::DisplayArrangement {
        let displays = self.arranged_displays(connected);
        let moves = displays
            .iter()
            .filter(|display| display.movable)
            .filter_map(|display| {
                let connector = display.connector.clone();
                self.output_binding(
                    &display.connector,
                    effect(DisplayEffect::MOVE_DISPLAY)
                        .map(move |(x, y): (i32, i32)| (connector.clone(), x, y)),
                )
                .map(|binding| (display.connector.clone(), binding))
            })
            .collect();
        super::display_arrangement::DisplayArrangement::new(
            displays,
            moves,
            self.arrangement_drag.clone(),
        )
        .with_selection(self.selected.clone())
        .with_pointed(self.pointed.clone())
        .identifying(self.identify_window)
    }

    /// The number each of `connected` is known by, the one Identify shows
    /// on it, in the same order.
    fn display_numbers(connected: &[&DisplayOutput]) -> Vec<usize> {
        settings_store::display_identify::number_displays(connected.iter().map(|output| {
            (
                output.builtin,
                if output.connector_label.is_empty() {
                    output.connector.as_str()
                } else {
                    output.connector_label.as_str()
                },
                output.connector.as_str(),
            )
        }))
    }

    /// The connected outputs as the arrangement canvas draws them: staged
    /// layout first, committed layout otherwise, numbered the way the
    /// compositor numbers them for Identify.
    fn arranged_displays(
        &self,
        connected: &[&DisplayOutput],
    ) -> Vec<super::display_arrangement::ArrangedDisplay> {
        use super::display_arrangement::{ArrangedDisplay, placement::Placement};

        let config = self
            .pending_candidate
            .as_ref()
            .unwrap_or(&self.output_config);
        let numbers = Self::display_numbers(connected);
        let mut next_free_x = 0_i32;
        let mut placed: Vec<ArrangedDisplay> = Vec::new();
        for (index, output) in connected.iter().enumerate() {
            let entry = config.outputs.get(&output.connector);
            let resolved = entry.cloned().unwrap_or_else(|| output.unconfigured());
            if !resolved.enabled {
                continue;
            }
            // Drawn at the logical size the pointer moves through, so a
            // 2x panel beside a 1x monitor looks, and snaps, as it behaves.
            let (width, height) = logical_extent_of(output, &resolved);
            // An output with no saved position has never been arranged: put
            // it after the others rather than on top of them.
            let (x, y) = if entry.is_some() {
                (resolved.x, resolved.y)
            } else {
                (next_free_x, 0)
            };
            next_free_x = next_free_x.max(x.saturating_add(width));
            placed.push(ArrangedDisplay {
                movement_group: None,
                shared: None,
                connector: output.connector.clone(),
                label: output_title(output),
                number: numbers.get(index).copied().unwrap_or(index + 1),
                placement: Placement::new(x, y, width, height),
                primary: resolved.primary,
                movable: resolved.mirror_of.is_none(),
            });
        }
        // A mirror shows its target's picture, so it sits on the target.
        let targets: Vec<(String, Placement)> = placed
            .iter()
            .map(|display| (display.connector.clone(), display.placement))
            .collect();
        for display in &mut placed {
            if let Some(target) = config
                .outputs
                .get(&display.connector)
                .and_then(|entry| entry.mirror_of.as_ref())
                && let Some((_, at)) = targets.iter().find(|(connector, _)| connector == target)
            {
                display.placement = display.placement.at(at.x, at.y);
            }
        }
        placed
    }

    /// Night Light: a comfort of every display, so it is not one display's
    /// setting and carries no brightness.
    fn build_night_light_card(&self) -> SettingsSection {
        let prefs = *self.prefs.get();
        section(
            &t("display-night-light"),
            rows(vec![
                night_light(&self.cx, &self.prefs),
                self.cx
                    .row(t("display-night-light-schedule"))
                    .described(t("display-night-light-schedule-desc"))
                    .id("display.night_light_schedule")
                    .toggle(
                        &LocalState::new(prefs.night_light_schedule),
                        crate::prefs::DisplayPrefs::paths().night_light_schedule,
                    ),
                self.cx
                    .row(t("display-night-light-warmth"))
                    .id("display.night_light_temperature")
                    .choice(
                        temperature_options(),
                        &LocalState::new(temperature_label(prefs.night_light_temperature)),
                        crate::prefs::DisplayPrefs::paths().night_light_temperature,
                        |label: String| {
                            temperature_from_label(&label)
                                .ok_or_else(|| format!("`{label}` is not a night light warmth"))
                        },
                    ),
                note(&t("display-night-light-caption")),
            ]),
        )
    }

    /// The selected display's brightness, where it can be controlled: the
    /// built-in panel's backlight (with automatic brightness beside it), or
    /// a monitor's over DDC/CI or dimmed by Tilo. Nothing for a display
    /// whose brightness cannot be set.
    fn build_output_brightness_rows(&self, output: &DisplayOutput) -> Vec<Item> {
        if output.builtin {
            let prefs = *self.prefs.get();
            let automatic_brightness = if self.ambient_sensor_available {
                automatic_brightness(&self.cx, prefs.auto_brightness)
            } else {
                value_row(&t("display-auto-brightness"), &t("display-no-ambient"))
            };
            return vec![
                match &self.brightness_error {
                    Some(error) => error_row(
                        &t("display-brightness"),
                        &t_args(
                            "display-brightness-error",
                            &[("error", error.as_str().into())],
                        ),
                    ),
                    None => self
                        .cx
                        .row(t("display-brightness"))
                        .stacked()
                        .id("display.brightness")
                        .slider_effect(
                            &LocalState::new(f64::from(self.brightness)),
                            effect(DisplayEffect::SET_BRIGHTNESS),
                        ),
                },
                automatic_brightness,
            ];
        }
        let Some(level) = self.output_brightness.get(&output.connector) else {
            return Vec::new();
        };
        let mut out_rows = vec![crate::widgets::choice_row_stacked(
            &t("display-brightness"),
            Box::new(self.slider(
                &output.connector,
                "brightness",
                f64::from(*level),
                effect(DisplayEffect::SET_MONITOR_BRIGHTNESS),
            )),
        )];
        if self.dimmed_by_tilo.contains(&output.connector) {
            out_rows.push(note(&t("display-brightness-dimmed-by-tilo")));
        }
        out_rows
    }

    /// The selected display's settings: brightness where it can be set,
    /// resolution, refresh rate, scale, rotation, the main display,
    /// mirroring and its place, then HDR, variable refresh and colour where
    /// the display offers them, and turning it off.
    fn build_output_card(
        &self,
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
    ) -> SettingsSection {
        let index = connected
            .iter()
            .position(|candidate| candidate.connector == output.connector)
            .unwrap_or_default();
        let config = self
            .staged()
            .outputs
            .get(&output.connector)
            .cloned()
            .unwrap_or_else(|| OutputConfig {
                primary: index == 0,
                x: i32::try_from(index)
                    .unwrap_or(i32::MAX)
                    .saturating_mul(1920),
                ..output.unconfigured()
            });
        let mut output_rows = self.build_output_brightness_rows(output);
        output_rows.extend(self.build_output_mode_rows(output, &config));
        output_rows.extend(self.build_output_primary_rows(output, connected, &config));
        output_rows.extend(self.build_output_capability_rows(output, &config));
        output_rows.push(setting_row(
            &t("display-use"),
            Box::new(self.toggle(
                &output.connector,
                "enabled",
                config.enabled,
                effect(DisplayEffect::USE_DISPLAY),
            )),
        ));
        section(&output_title(output), rows(output_rows))
    }

    /// Scale, rotation, the main display and mirroring. Where the display
    /// sits is the arrangement canvas's alone (by hand or arrow keys).
    fn build_output_primary_rows(
        &self,
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
        config: &OutputConfig,
    ) -> Vec<Item> {
        let scale_choices: Vec<String> = DISPLAY_SCALES
            .iter()
            .map(|(label, _)| (*label).into())
            .collect();
        let mirror_choices = self.relative_choices(output, true);
        let mirror_placeholder = config
            .mirror_of
            .as_ref()
            .and_then(|target| {
                connected
                    .iter()
                    .find(|candidate| candidate.connector == *target)
            })
            .map_or_else(
                || "Extend workspace".into(),
                |target| format!("Mirror {}", target.label),
            );
        vec![
            setting_row(
                &t("display-scale"),
                Box::new(self.choice(
                    &output.connector,
                    "scale",
                    scale_choices,
                    scale_label(config.scale),
                    effect(DisplayEffect::SET_OUTPUT_SCALE),
                )),
            ),
            setting_row(
                &t("display-rotation"),
                Box::new(self.choice(
                    &output.connector,
                    "transform",
                    transform_options(),
                    &transform_label(config.transform),
                    effect(DisplayEffect::ROTATE_DISPLAY),
                )),
            ),
            setting_row(
                &t("display-primary"),
                Box::new({
                    let display = output.connector.clone();
                    self.bind_output(
                        &output.connector,
                        Toggle::from_state(&LocalState::new(config.primary))
                            .with_id(&Self::id(&output.connector, "primary")),
                        effect(DisplayEffect::SET_PRIMARY_DISPLAY)
                            .map(move |_: bool| display.clone()),
                    )
                }),
            ),
            setting_row(
                &t("display-mirroring"),
                Box::new(self.choice(
                    &output.connector,
                    "mirror",
                    mirror_choices,
                    &mirror_placeholder,
                    effect(DisplayEffect::MIRROR_DISPLAY),
                )),
            ),
        ]
    }

    /// Why the display is dark, if it is; then its resolution and, for that
    /// resolution, its refresh rate. Both choose one of the display's modes.
    fn build_output_mode_rows(&self, output: &DisplayOutput, config: &OutputConfig) -> Vec<Item> {
        let mut out_rows = Vec::new();
        if let Some(reason) = output.unavailable_reason {
            out_rows.push(value_row(
                &t("display-status"),
                &t(output_unavailable_key(reason)),
            ));
        }
        let Some(current) = config
            .mode
            .or(output.current_mode)
            .or_else(|| output.modes.first().copied())
        else {
            out_rows.push(value_row(
                &t("display-resolution"),
                &t("display-modes-unavailable"),
            ));
            return out_rows;
        };
        if output.modes.is_empty() {
            out_rows.push(value_row(
                &t("display-resolution"),
                &resolution_label(current),
            ));
            out_rows.push(value_row(
                &t("display-refresh-rate"),
                &refresh_label(current),
            ));
            return out_rows;
        }
        let modes = output.modes.clone();
        let resolutions = resolution_choices(&modes);
        let rates = refresh_choices(&modes, current);
        let target = output.connector.clone();
        let for_resolution = modes.clone();
        out_rows.push(setting_row(
            &t("display-resolution"),
            Box::new(
                self.bind_output(
                    &output.connector,
                    Dropdown::from_state(resolutions, &LocalState::new(resolution_label(current)))
                        .with_id(&Self::id(&output.connector, "mode"))
                        .with_placeholder(&resolution_label(current)),
                    effect(DisplayEffect::SET_DISPLAY_MODE).map(move |resolution: String| {
                        (
                            target.clone(),
                            mode_for_resolution(&for_resolution, current, &resolution)
                                .map_or(resolution, OutputMode::label),
                        )
                    }),
                ),
            ),
        ));
        let target = output.connector.clone();
        out_rows.push(setting_row(
            &t("display-refresh-rate"),
            Box::new(
                self.bind_output(
                    &output.connector,
                    Dropdown::from_state(rates, &LocalState::new(refresh_label(current)))
                        .with_id(&Self::id(&output.connector, "refresh"))
                        .with_placeholder(&refresh_label(current)),
                    effect(DisplayEffect::SET_DISPLAY_MODE).map(move |rate: String| {
                        (
                            target.clone(),
                            mode_for_refresh(&modes, current, &rate)
                                .map_or(rate, OutputMode::label),
                        )
                    }),
                ),
            ),
        ));
        out_rows
    }

    /// HDR / variable-refresh-rate / colour-profile rows.
    fn build_output_capability_rows(
        &self,
        output: &DisplayOutput,
        config: &OutputConfig,
    ) -> Vec<Item> {
        let mut out_rows = Vec::new();

        if output.hdr {
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr"),
                &crate::i18n::t("display-hdr-description"),
                Box::new(self.toggle(
                    &output.connector,
                    "hdr",
                    config.hdr,
                    effect(DisplayEffect::SET_HDR),
                )),
            ));
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr-sdr-brightness"),
                &crate::i18n::t("display-hdr-sdr-brightness-description"),
                crate::widgets::trailing_slider(Box::new(self.slider(
                    &output.connector,
                    "sdr_brightness",
                    sdr_brightness_fraction(config.sdr_brightness_nits),
                    effect(DisplayEffect::SET_SDR_BRIGHTNESS),
                ))),
            ));
        } else {
            out_rows.push(value_row(
                &crate::i18n::t("display-hdr"),
                &crate::i18n::t(hdr_unavailable_key(
                    output.sink_hdr,
                    output.hdr_unavailable_reason,
                )),
            ));
        }
        if output.vrr {
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-vrr"),
                &crate::i18n::t("display-vrr-description"),
                Box::new(self.toggle(
                    &output.connector,
                    "vrr",
                    config.vrr,
                    effect(DisplayEffect::SET_VRR),
                )),
            ));
        } else {
            out_rows.push(value_row(
                &crate::i18n::t("display-vrr"),
                &crate::i18n::t(adaptive_sync_unavailable_key(
                    output.sink_vrr,
                    output.vrr_unavailable_reason,
                )),
            ));
        }
        if output.color_profiles.is_empty() {
            out_rows.push(value_row(
                &t("display-colour-profile"),
                &t("display-colour-profile-empty"),
            ));
        } else {
            let mut choices = vec!["System default".into()];
            choices.extend(output.color_profiles.iter().filter_map(|id| {
                output
                    .color_profile_labels
                    .get(id)
                    .map(|label| format!("{label} ({id})"))
            }));
            let placeholder = config
                .color_profile
                .as_ref()
                .and_then(|id| {
                    output
                        .color_profile_labels
                        .get(id)
                        .map(|label| format!("{label} ({id})"))
                })
                .unwrap_or_else(|| "System default".into());
            out_rows.push(setting_row(
                &t("display-colour-profile"),
                Box::new(self.choice(
                    &output.connector,
                    "color_profile",
                    choices,
                    &placeholder,
                    effect(DisplayEffect::SET_COLOR_PROFILE),
                )),
            ));
        }
        out_rows
    }

    /// The safety/apply-transaction card, driven by the on-disk
    /// [`DisplayTransaction`] state machine.
    fn build_safety_card(&self) -> Option<SettingsSection> {
        let transaction = self.transaction();
        match transaction.as_ref().map(|transaction| transaction.state) {
            Some(DisplayTransactionState::AwaitingConfirmation) => {
                let seconds = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.deadline_unix_ms)
                    .map(|deadline| deadline.saturating_sub(unix_now_ms()).div_ceil(1_000))
                    .unwrap_or_default();
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                // One question, one pair of answers.
                let revert = self.cx.bind_invocation(
                    ui::widget::Button::new("settings.display.revert", &t("display-revert-now")),
                    DisplayEffect::RevertDisplayChanges,
                );
                let keep = self.cx.bind_invocation(
                    ui::widget::Button::new("settings.display.keep", &t("display-keep"))
                        .role(ui::widget::ButtonRole::Primary),
                    DisplayEffect::KeepDisplayChanges,
                );
                Some(section(
                    &t("display-keep-title"),
                    rows(vec![crate::widgets::item_row(
                        &t_args("display-keep-caption", &[("seconds", seconds.into())]),
                        Vec::new(),
                        &[],
                        vec![revert, keep],
                    )]),
                ))
            }
            Some(
                DisplayTransactionState::Requested
                | DisplayTransactionState::ConfirmRequested
                | DisplayTransactionState::RevertRequested,
            ) => Some(section(
                &t("display-applying-title"),
                rows(vec![note(&t("display-applying-caption"))]),
            )),
            Some(DisplayTransactionState::Rejected) => {
                let mut content = vec![note(&t("display-rejected-caption"))];
                if self.pending_candidate.is_some() {
                    let try_again = t("display-try-again");
                    content.push(self.cx.row(t("display-retry")).button(
                        "settings.display.apply",
                        &try_again,
                        DisplayEffect::ApplyDisplayChanges,
                    ));
                }
                Some(section(&t("display-rejected-title"), rows(content)))
            }
            _ if self.pending_candidate.is_some() => {
                let apply = t("display-apply");
                Some(section(
                    &t("display-apply-title"),
                    rows(vec![
                        self.cx
                            .row(t("display-apply-row"))
                            .described(t("display-apply-caption"))
                            .button(
                                "settings.display.apply",
                                &apply,
                                DisplayEffect::ApplyDisplayChanges,
                            ),
                    ]),
                ))
            }
            // Nothing staged and nothing being tried: no card.
            _ => None,
        }
    }

    /// Layout tools whose display changes are kept without asking, each
    /// with a way to be asked again. Absent while no tool is trusted.
    fn build_trusted_tools_card(&self) -> Option<SettingsSection> {
        if !self.of_device {
            return None;
        }
        let trusted = settings_store::display_tools::TrustedDisplayTools::load();
        if trusted.tools.is_empty() {
            return None;
        }
        let mut content = vec![note(&crate::i18n::t("display-tools-caption"))];
        for tool in &trusted.tools {
            content.push(self.cx.row(tool.name()).button(
                "settings.display.tool.forget",
                &crate::i18n::t("display-tools-forget"),
                DisplayEffect::ForgetDisplayTool {
                    tool: tool.executable.clone(),
                },
            ));
        }
        Some(section(
            &crate::i18n::t("display-tools-title"),
            rows(content),
        ))
    }
}

impl PaneModel for DisplayModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_device_move();
        self.poll_background();
        self.poll_device();
        self.poll_selection();
        if self.automatic_display_prefs.refresh() {
            self.changed = true;
        }
        if self.pending_change.refresh() {
            // Kept, reverted or rejected, here or in the shell's own
            // prompt: the page shows what now stands.
            self.changed = true;
            if let Some(transaction) = self.transaction() {
                self.settle(&transaction);
            }
        }
        if self.shared_display_prefs.refresh() {
            self.changed = true;
        }
        if self.prefs.refresh() {
            self.changed = true;
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "DisplayPane"
    }

    fn busy(&self) -> bool {
        self.hardware_probe.is_some()
            || self.pending_brightness.is_some()
            || self.awaiting_device_move.is_some()
    }

    fn next_wake(&self) -> Option<Instant> {
        let followed = self
            .following
            .upgrade()
            .is_some_and(|watched| watched.is_complete());
        (self.of_device && (!followed || self.settling))
            .then(|| self.last_snapshot_check + Duration::from_secs(1))
    }

    fn follow(&mut self) -> Option<Rc<dyn std::any::Any>> {
        if !self.of_device {
            return None;
        }
        let files: Vec<std::path::PathBuf> = [
            settings_store::hardware::display_snapshot_path(),
            settings_store::device_displays::path(),
            settings_store::person_displays::path(),
        ]
        .into_iter()
        .flatten()
        .collect();
        // The pending display change is followed through the settings
        // document ([`PendingDisplayChange`]), never by watching a file.
        let watched =
            crate::watch::shared(&mut self.following, || crate::watch::Watched::files(&files));
        Some(watched)
    }
}

/// Whether `transaction` still waits for the compositor or the person.
fn awaits_answer(transaction: &DisplayTransaction) -> bool {
    matches!(
        transaction.state,
        DisplayTransactionState::Requested
            | DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::ConfirmRequested
            | DisplayTransactionState::RevertRequested
    )
}

/// The connected output `display`.
fn output_of<'a>(
    connected: &'a [DisplayOutput],
    display: &str,
) -> Result<&'a DisplayOutput, String> {
    connected
        .iter()
        .find(|output| output.connector == display)
        .ok_or_else(|| "that display is no longer connected".to_string())
}

/// Place `display` beside another as `position` says (`Right of …`), by
/// the neighbour's logical extent: a 2x panel's right-hand neighbour starts
/// 1280 in, not 2560.
fn place_beside(
    display: &str,
    position: &str,
    cfg: &mut OutputConfig,
    next: &DisplayConfig,
    connected: &[DisplayOutput],
) -> Result<(), String> {
    let output = output_of(connected, display)?;
    if position == "Automatic" {
        return Ok(());
    }
    let (side, reference) = ["Left", "Right", "Above", "Below"]
        .iter()
        .find_map(|side| {
            let label = position.strip_prefix(&format!("{side} of "))?;
            let target = connected
                .iter()
                .find(|candidate| candidate.label == label && candidate.connector != display)?;
            Some((*side, target))
        })
        .ok_or_else(|| format!("`{position}` is not a place beside another display"))?;
    let reference_cfg = next
        .outputs
        .get(&reference.connector)
        .cloned()
        .unwrap_or_else(|| reference.unconfigured());
    let (width, height) = logical_extent_of(output, cfg);
    let (reference_width, reference_height) = logical_extent_of(reference, &reference_cfg);
    (cfg.x, cfg.y) = match side {
        "Left" => (reference_cfg.x.saturating_sub(width), reference_cfg.y),
        "Right" => (
            reference_cfg.x.saturating_add(reference_width),
            reference_cfg.y,
        ),
        "Above" => (reference_cfg.x, reference_cfg.y.saturating_sub(height)),
        _ => (
            reference_cfg.x,
            reference_cfg.y.saturating_add(reference_height),
        ),
    };
    Ok(())
}

/// Exactly one enabled display is primary: `preferred` when given, else the
/// one already primary, else the first enabled one.
fn choose_primary(next: &mut DisplayConfig, connected: &[DisplayOutput], preferred: Option<&str>) {
    let enabled = |connector: &str| {
        next.outputs
            .get(connector)
            .is_some_and(|config| config.enabled)
    };
    let selected = preferred
        .filter(|connector| enabled(connector))
        .map(str::to_owned)
        .or_else(|| {
            connected.iter().find_map(|output| {
                next.outputs
                    .get(&output.connector)
                    .filter(|config| config.enabled && config.primary)
                    .map(|_| output.connector.clone())
            })
        })
        .or_else(|| {
            connected
                .iter()
                .find(|output| enabled(&output.connector))
                .map(|output| output.connector.clone())
        });
    if let Some(selected) = selected {
        for output in connected {
            if let Some(config) = next.outputs.get_mut(&output.connector) {
                config.primary = output.connector == selected;
            }
        }
    }
}

/// Mirroring is one viewport sampled by two sinks: pick the best pixel
/// extent both advertise, then each sink's best refresh for that extent.
fn mirror_on_shared_modes(next: &mut DisplayConfig, connected: &[DisplayOutput]) {
    let pairs: Vec<(String, String)> = next
        .outputs
        .iter()
        .filter_map(|(connector, config)| {
            config
                .mirror_of
                .as_ref()
                .map(|target| (connector.clone(), target.clone()))
        })
        .collect();
    for (connector, target) in pairs {
        let (Some(source), Some(sink)) = (
            connected
                .iter()
                .find(|output| output.connector == connector),
            connected.iter().find(|output| output.connector == target),
        ) else {
            continue;
        };
        if let Some((source_mode, sink_mode)) = best_mirror_modes(source, sink) {
            if let Some(config) = next.outputs.get_mut(&connector) {
                config.mode = Some(source_mode);
            }
            if let Some(config) = next.outputs.get_mut(&target) {
                config.mode = Some(sink_mode);
            }
        }
    }
}

#[cfg(test)]
mod tests;
