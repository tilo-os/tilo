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
    setting_row_described, slider_row, value_row,
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
                        "identify_requested_unix_ms": model.host_state.as_ref().and_then(|state| state.identify).map(|request| request.requested_unix_ms),
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

/// Present one compositor descriptor without leaking a persistence collision
/// into the UI. A unique connector remains the familiar `HDMI-A-1`; two
/// cards with that label become e.g. `HDMI-A-1 on pci:0000:01:00.0` so every
/// control, mirror target and saved key refers to exactly one display.
fn snapshot_label(descriptor: &OutputDescriptor, builtin: bool) -> String {
    let monitor_name = descriptor
        .monitor
        .as_ref()
        .map(MonitorIdentity::display_name)
        .filter(|name| !name.is_empty());
    let base = match (builtin, monitor_name) {
        (true, _) => "Built-in display".to_string(),
        // The port stays visible: two identical monitors differ only there.
        (false, Some(name)) => format!("{name} on {}", descriptor.label()),
        (false, None) => descriptor.label().to_string(),
    };
    descriptor
        .card_qualifier()
        .map_or_else(|| base.clone(), |card| format!("{base} on {card}"))
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
    let mut out: Vec<_> = snapshot
        .outputs
        .iter()
        .cloned()
        .map(|snapshot| {
            let label = snapshot_label(&snapshot.descriptor, snapshot.builtin);
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
    /// The arrangement canvas's in-progress drag; kept here because the
    /// composition is rebuilt between frames.
    arrangement_drag: LocalState<Option<super::display_arrangement::ArrangementDrag>>,
    device_arrangement_drag: LocalState<Option<super::display_arrangement::ArrangementDrag>>,
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
        Self {
            shared_display_prefs: cx.follow::<shared_displays::SharedDisplayPrefs>(),
            automatic_display_prefs: cx.follow::<shared_displays::AutomaticDisplayPrefs>(),
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
            arrangement_drag: LocalState::new(None),
            device_arrangement_drag: LocalState::new(None),
            person_devices: settings_store::person_displays::PersonDisplays::load()
                .map_or_else(Vec::new, |view| view.devices),
            device_displays: settings_store::device_displays::DeviceDisplaySnapshot::load()
                .map_or_else(Vec::new, |snapshot| snapshot.displays),
            changed: false,
        }
    }

    /// A built-in panel at 62 %, with an ambient light sensor; nothing it
    /// does reaches the device or its files.
    fn sample(cx: PaneCx) -> Self {
        let device_displays = cx
            .scene()
            .map_or_else(Vec::new, |scene| scene.device_displays.clone());
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let mut model = Self::detached(cx).with_outputs(vec![DisplayOutput {
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
        }]);
        model.person_devices = model
            .cx
            .scene()
            .map_or_else(Vec::new, |scene| scene.person_devices.clone());
        model.device_displays = device_displays;
        model
    }

    /// A model with no outputs whose brightness changes nothing; tests give
    /// it what they drive.
    fn detached(cx: PaneCx) -> Self {
        Self {
            shared_display_prefs: cx.follow::<shared_displays::SharedDisplayPrefs>(),
            automatic_display_prefs: cx.follow::<shared_displays::AutomaticDisplayPrefs>(),
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
            arrangement_drag: LocalState::new(None),
            device_arrangement_drag: LocalState::new(None),
            device_displays: Vec::new(),
            person_devices: Vec::new(),
            changed: false,
        }
    }

    #[must_use]
    fn with_outputs(mut self, outputs: Vec<DisplayOutput>) -> Self {
        self.outputs = outputs;
        self
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

    /// Observed output timing stays separate from a requested candidate.
    fn transaction(&self) -> Option<DisplayTransaction> {
        if let Some(state) = &self.host_state {
            state.transaction.clone()
        } else {
            self.of_device.then(DisplayTransaction::load).flatten()
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
            Slider::from_state(&LocalState::new(value)).with_id(&Self::id(display, field)),
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
                    .map(|_| "Each display shows its number.".into())
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
        let notice = match transaction.state {
            DisplayTransactionState::Confirmed => "Display changes kept.".to_owned(),
            DisplayTransactionState::Reverted => "Display changes reverted.".to_owned(),
            DisplayTransactionState::Rejected => transaction
                .message
                .clone()
                .unwrap_or_else(|| "Display configuration was rejected.".into()),
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
        let arrangement = self.build_arrangement_card(&connected);
        let brightness = self.build_brightness_card();

        // What is being tried, or waits to be, leads: its countdown must be
        // seen. Then the arrangement and the brightness people reach for
        // most, each display's own settings, and the displays of other
        // devices.
        let mut cards: Vec<SettingsSection> = self.build_safety_card().into_iter().collect();
        cards.push(arrangement);
        if let Some(notice) = &self.notice {
            cards.push(section(&t("display-card-title"), rows(vec![note(notice)])));
        }
        cards.push(brightness);
        for (index, output) in connected.iter().enumerate() {
            cards.push(self.build_output_card(index, output, &connected));
        }
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

    /// The "Displays" arrangement card: a drag-to-arrange canvas of the
    /// connected outputs with an Identify button, plus a caption about idle
    /// ports. See [`Self::connected_outputs`] for why disconnected ports
    /// aren't enumerated.
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
        let mut out_rows: Vec<Item> = vec![
            block(Box::new(
                super::display_arrangement::DisplayArrangement::new(
                    displays,
                    moves,
                    self.arrangement_drag.clone(),
                ),
            )),
            note(&crate::i18n::t("display-arrangement-hint")),
            self.cx.row(crate::i18n::t("display-identify-row")).button(
                "settings.display.identify",
                &crate::i18n::t("display-identify"),
                DisplayEffect::IdentifyDisplays,
            ),
        ];
        // One honest line about the idle ports rather than a row each.
        out_rows.push(note(&if connected.len() > 1 {
            t("display-caption-switch")
        } else if disconnected == 0 {
            t("display-caption-second")
        } else {
            t("display-caption-port")
        }));
        section(&t("display-card-title"), rows(out_rows))
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
        let numbers =
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
            }));
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

    /// The brightness slider + night-light comfort preferences card.
    fn build_brightness_card(&self) -> SettingsSection {
        let prefs = *self.prefs.get();
        let automatic_brightness = if self.ambient_sensor_available {
            self.cx
                .row(t("display-auto-brightness"))
                .id("display.auto_brightness")
                .toggle_effect(
                    &LocalState::new(prefs.auto_brightness),
                    effect(DisplayEffect::SET_AUTOMATIC_BRIGHTNESS),
                )
        } else {
            value_row(&t("display-auto-brightness"), &t("display-no-ambient"))
        };
        section(
            &t("display-brightness"),
            rows(vec![
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
                self.cx
                    .row(t("display-night-light"))
                    .id("display.night_light")
                    .toggle(
                        &LocalState::new(prefs.night_light),
                        crate::prefs::DisplayPrefs::paths().night_light,
                    ),
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
                                .ok_or_else(|| format!("`{label}` is not a Night Light warmth"))
                        },
                    ),
                note(&t("display-night-light-caption")),
            ]),
        )
    }

    /// The full per-output card: identity/arrangement rows, mode/rotation
    /// rows, then capability rows (HDR/VRR/colour profile).
    fn build_output_card(
        &self,
        index: usize,
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
    ) -> SettingsSection {
        let config = self
            .pending_candidate
            .as_ref()
            .unwrap_or(&self.output_config)
            .outputs
            .get(&output.connector)
            .cloned()
            .unwrap_or_else(|| OutputConfig {
                primary: index == 0,
                x: index as i32 * 1920,
                ..output.unconfigured()
            });
        let mut output_rows = Vec::new();
        if self.output_brightness.contains_key(&output.connector) {
            output_rows.push(slider_row(
                &t("display-brightness"),
                Box::new(
                    self.slider(
                        &output.connector,
                        "brightness",
                        f64::from(
                            self.output_brightness
                                .get(&output.connector)
                                .copied()
                                .unwrap_or_default(),
                        ),
                        effect(DisplayEffect::SET_MONITOR_BRIGHTNESS),
                    ),
                ),
            ));
            if self.dimmed_by_tilo.contains(&output.connector) {
                output_rows.push(note(&t("display-brightness-dimmed-by-tilo")));
            }
        }
        output_rows.extend(self.build_output_primary_rows(output, connected, &config));
        output_rows.extend(self.build_output_mode_and_rotation_rows(output, &config));
        output_rows.extend(self.build_output_capability_rows(output, &config));
        section(&output_title(output), rows(output_rows))
    }

    /// Enable/primary toggles + scale/position/mirroring dropdowns.
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
        let position_choices = self.relative_choices(output, false);
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
        let position_current = "Automatic".to_owned();
        vec![
            setting_row(
                &t("display-use"),
                Box::new(self.toggle(
                    &output.connector,
                    "enabled",
                    config.enabled,
                    effect(DisplayEffect::USE_DISPLAY),
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
                &t("display-position"),
                Box::new(self.choice(
                    &output.connector,
                    "position",
                    position_choices,
                    &position_current,
                    effect(DisplayEffect::PLACE_DISPLAY),
                )),
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

    /// Resolution/refresh-rate dropdown (or an unavailable note) + rotation.
    fn build_output_mode_and_rotation_rows(
        &self,
        output: &DisplayOutput,
        config: &OutputConfig,
    ) -> Vec<Item> {
        let mut out_rows = Vec::new();
        if let Some(reason) = output.unavailable_reason {
            out_rows.push(value_row(
                &t("display-status"),
                &t(output_unavailable_key(reason)),
            ));
        }
        if output.modes.is_empty() {
            out_rows.push(value_row(
                "Resolution & refresh rate",
                "Compositor capability data unavailable",
            ));
        } else {
            let mode_choices: Vec<String> = output
                .modes
                .iter()
                .copied()
                .map(OutputMode::label)
                .collect();
            let mode_placeholder = config
                .mode
                .or(output.current_mode)
                .map_or_else(|| "Preferred".into(), OutputMode::label);
            out_rows.push(setting_row(
                "Resolution & refresh rate",
                Box::new(self.choice(
                    &output.connector,
                    "mode",
                    mode_choices,
                    &mode_placeholder,
                    effect(DisplayEffect::SET_DISPLAY_MODE),
                )),
            ));
        }
        let rotation = transform_options();
        let rotation_current = transform_label(config.transform);
        out_rows.push(setting_row(
            &t("display-rotation"),
            Box::new(self.choice(
                &output.connector,
                "transform",
                rotation,
                &rotation_current,
                effect(DisplayEffect::ROTATE_DISPLAY),
            )),
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
            ) => {
                let applying = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.message.as_deref())
                    .map_or_else(|| t("display-applying-caption"), str::to_string);
                Some(section(
                    &t("display-applying-title"),
                    rows(vec![note(&applying)]),
                ))
            }
            Some(DisplayTransactionState::Rejected) => {
                let rejected = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.message.as_deref())
                    .map_or_else(|| t("display-rejected-caption"), str::to_string);
                let mut content = vec![note(&rejected)];
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
        if self.automatic_display_prefs.refresh() {
            self.changed = true;
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
            // The pending transaction is a section of the settings file.
            settings_store::settings_path(),
        ]
        .into_iter()
        .flatten()
        .collect();
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
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    /// The compositor's exact output is the shared control and automation target.
    #[test]
    fn peer_drag_exports_the_same_exact_request_without_editing_local_outputs() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        let saved = pane.output_config.clone();
        pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
            device: "drag-peer".into(),
            device_label: "Laptop".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: true,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 9,
        }];
        let row = DisplayModel::device_output_row(&pane.device_displays[0]);
        assert_eq!(row.id(), "settings.device-display:drag-peer/eDP-1");
        assert!(
            pane.apply(&DisplayEffect::MoveDeviceDisplay {
                display: "drag-peer/eDP-1".into(),
                revision: 8,
                x: 0,
                y: 1080
            })
            .is_err(),
            "a drag bound before a new peer layout cannot overwrite it"
        );
        pane.apply(&DisplayEffect::MoveDeviceDisplay {
            display: "drag-peer/eDP-1".into(),
            revision: 9,
            x: 0,
            y: 1080,
        })
        .unwrap();
        let ui::widget::AppHostIntent::MoveDeviceDisplay { request, reply } =
            pane.take_host_intent().unwrap()
        else {
            panic!("exact typed host request");
        };
        assert_eq!(request.screen, pane.device_displays[0]);
        assert_eq!((request.x, request.y), (0, 1080));
        assert_eq!(pane.output_config, saved);
        assert!(pane.pending_candidate.is_none());
        assert!(pane.take_host_intent().is_none());
        reply.upgrade().unwrap().borrow_mut().result = Some(Err("Disconnected".into()));
        pane.poll_device_move();
        assert_eq!(pane.notice.as_deref(), Some("Disconnected"));
        pane.device_displays[0].online = false;
        assert!(
            pane.apply(&DisplayEffect::MoveDeviceDisplay {
                display: "drag-peer/eDP-1".into(),
                revision: 9,
                x: 0,
                y: 0
            })
            .is_err()
        );
    }

    #[test]
    fn peer_drag_confirmation_reports_owner_updates_competing_layouts_and_offline() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        for outcome in 0..3 {
            let mut pane = laptop_with_monitor();
            pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
                device: "confirmation-peer".into(),
                device_label: "Peer".into(),
                connector: "eDP-1".into(),
                output_id: None,
                label: "Panel".into(),
                online: true,
                local: false,
                x: 1920,
                y: 0,
                width: 1280,
                height: 720,
                scale_milli: 1250,
                revision: 9,
            }];
            pane.apply(&DisplayEffect::MoveDeviceDisplay {
                display: "confirmation-peer/eDP-1".into(),
                revision: 9,
                x: 0,
                y: 1080,
            })
            .unwrap();
            let ui::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } =
                pane.take_host_intent().unwrap()
            else {
                panic!("placement host request");
            };
            reply.upgrade().unwrap().borrow_mut().result = Some(Ok(()));
            pane.poll_device_move();
            assert!(
                pane.awaiting_device_move.is_some(),
                "dispatch is never reported as applied"
            );
            let screen = &mut pane.device_displays[0];
            screen.revision = 10;
            let expected = match outcome {
                0 => {
                    screen.x = 0;
                    screen.y = 1080;
                    "display-device-move-confirmed"
                }
                1 => {
                    screen.y = 20;
                    "display-device-move-changed"
                }
                _ => {
                    screen.online = false;
                    "display-device-move-unavailable"
                }
            };
            pane.poll_device_move();
            assert!(pane.awaiting_device_move.is_none());
            assert_eq!(pane.notice, Some(t(expected)));
        }
    }

    #[test]
    fn peer_drag_timeout_cancels_an_unadmitted_host_gesture() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
            device: "timeout-peer".into(),
            device_label: "Peer".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: true,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 9,
        }];
        pane.apply(&DisplayEffect::MoveDeviceDisplay {
            display: "timeout-peer/eDP-1".into(),
            revision: 9,
            x: 0,
            y: 1080,
        })
        .unwrap();
        let ui::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } =
            pane.take_host_intent().unwrap()
        else {
            panic!("placement host request");
        };
        pane.awaiting_device_move.as_mut().unwrap().1 = ui::time::now()
            .checked_sub(Duration::from_secs(11))
            .unwrap();
        pane.poll_device_move();
        assert!(
            reply.upgrade().is_none(),
            "an expired gesture leaves no live admission witness"
        );
        assert!(pane.pending_device_move.is_none() && pane.awaiting_device_move.is_none());
        assert_eq!(pane.notice, Some(t("display-device-move-unconfirmed")));
        assert!(pane.take_host_intent().is_none());
    }

    #[test]
    fn peer_drag_uses_the_mounted_app_semantic_door_and_fixed_revision() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        app.call(
            "open",
            serde_json::json!({
                "anchor": crate::effects::pane_row_id(crate::categories::Category::Displays)
            }),
        )
        .unwrap();
        app.surface::<SettingsApp, _>(|settings| {
            crate::panes::display::area(settings)
                .unwrap()
                .model()
                .borrow_mut()
                .device_displays = vec![settings_store::device_displays::DeviceDisplay {
                device: "semantic-peer".into(),
                device_label: "Peer laptop".into(),
                connector: "eDP-1".into(),
                output_id: None,
                label: "Panel".into(),
                online: true,
                local: false,
                x: 1920,
                y: 0,
                width: 1280,
                height: 720,
                scale_milli: 1250,
                revision: 9,
            }];
        })
        .unwrap();
        app.settle();
        let address = "settings.device-display:semantic-peer/eDP-1";
        let surface = app.agent_surface();
        let row = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == address && row["action"] == "move-device-display")
            .expect("discovered screen exports its move action");
        assert_eq!(
            row["params"].as_array().unwrap().len(),
            2,
            "only logical x/y are caller-controlled"
        );
        assert!(
            app.call_on(
                address,
                "move-device-display",
                serde_json::json!({"x":0,"y":1080,"revision":1})
            )
            .is_err()
        );
        assert!(
            app.call_on(
                address,
                "move-device-display",
                serde_json::json!({"x":0,"y":1080,"display":"other/eDP-1"})
            )
            .is_err()
        );
        app.call_on(
            address,
            "move-device-display",
            serde_json::json!({"x":0,"y":1080}),
        )
        .unwrap();
        let intent = app.surface::<SettingsApp, _>(<SettingsApp as ui::framework::ResourceSurface<Settings, SettingsEffect>>::take_host_intent).flatten().expect("the real mounted surface carries the typed host intent");
        let ui::widget::AppHostIntent::MoveDeviceDisplay { request, .. } = intent else {
            panic!("typed topology request");
        };
        assert_eq!(request.screen.device, "semantic-peer");
        assert_eq!(request.screen.revision, 9);
        assert_eq!((request.x, request.y), (0, 1080));
        assert!(app.harness().component_contract_violations().is_empty());
    }

    #[test]
    fn connected_display_controls_publish_their_parameter_actions() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new()
            .expect("settings opens headlessly");
        app.settle();
        let open = crate::categories::Category::Displays;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
        .expect("Displays opens through its declared operation");
        app.settle();
        let surface = app.agent_surface();
        let rows = surface.as_array().expect("published actions");
        for (action, field) in [
            ("set-display-mode", "mode"),
            ("set-output-scale", "scale"),
            ("rotate-display", "transform"),
            ("use-display", "enabled"),
            ("set-primary-display", "primary"),
            ("place-display", "position"),
            ("mirror-display", "mirror"),
        ] {
            assert!(
                rows.iter()
                    .any(|row| row["resource"] == "settings.display-output:eDP-1"
                        && row["action"] == action),
                "{action} is published on the connected display: {surface}"
            );
            let bindings = app
                .surface::<SettingsApp, _>(|settings| {
                    ui::widget::controls::control_action_bindings(&*settings)
                })
                .expect("mounted Settings");
            assert!(bindings.iter().any(|binding| matches!(&binding.kind,
                ui::widget::ControlBindingKind::Action(address)
                    if address.resource.as_str() == "settings.display-output:eDP-1" && address.action == action)
                && binding.control_id == format!("display.output.eDP-1.{field}")),
                "{field} shares the published display operation: {bindings:#?}");
        }
        assert!(app.harness().component_contract_violations().is_empty());
    }

    /// Only the asked value is caller-controlled; the row owns the output identity.
    #[test]
    fn display_inspection_and_staging_share_the_production_door() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new()
            .expect("settings opens headlessly");
        app.settle();
        let before = app
            .call("inspect-display-configuration", serde_json::json!({}))
            .expect("display observation is declared")
            .expect("observation returned");
        assert_eq!(before["has_staged_changes"], false);
        let mode = before["observed_outputs"][0]["supported_modes"][0].clone();
        // Read the declared choices instead of inventing a refresh-rate spelling.
        let surface = app.agent_surface();
        let row = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row["resource"] == "settings.display-output:eDP-1"
                    && row["action"] == "set-display-mode"
            })
            .expect("the connected output publishes mode selection");
        let choice = row["params"][0]["choices"][0]
            .as_str()
            .expect("actual mode choice");
        app.call_on(
            "settings.display-output:eDP-1",
            "set-display-mode",
            serde_json::json!({"mode":choice}),
        )
        .expect("the bound row stages its own output");
        app.call_on(
            "settings.display-output:eDP-1",
            "set-output-scale",
            serde_json::json!({"scale":"200%"}),
        )
        .expect("a real scale change stages through the same output resource");
        app.settle();
        let staged = app
            .call("inspect-display-configuration", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(staged["has_staged_changes"], true);
        assert_eq!(
            staged["staged_configuration"]["outputs"]["eDP-1"]["scale"],
            2.0
        );
        assert_eq!(
            staged["staged_configuration"]["outputs"]["eDP-1"]["mode"],
            mode
        );
        assert_eq!(staged["observed_outputs"], before["observed_outputs"]);
        assert_eq!(staged["saved_configuration"], before["saved_configuration"]);
        assert!(
            app.call_on(
                "settings.display-output:eDP-1",
                "set-display-mode",
                serde_json::json!({"mode":choice,"display":"another-output"})
            )
            .is_err(),
            "a caller cannot replace the row's output identity"
        );
        assert!(
            app.call_on(
                "settings.display-output:eDP-1",
                "set-display-mode",
                serde_json::json!({"mode":"unsupported refresh rate"})
            )
            .is_err(),
            "unsupported modes are rejected by the published contract"
        );
        let after = app
            .call("inspect-display-configuration", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(
            after, staged,
            "inspection and refused invocations cannot edit display state"
        );
    }

    #[test]
    fn display_observation_is_available_to_viewers_without_staging_rights() {
        let mut app =
            ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
                .expect("read-only Settings opens");
        app.settle();
        let before = app
            .call("inspect-display-configuration", serde_json::json!({}))
            .expect("read-only observer is available")
            .expect("observation returned");
        assert!(
            app.call_on(
                "settings.display-output:eDP-1",
                "set-output-scale",
                serde_json::json!({"scale":"200%"})
            )
            .is_err(),
            "inspection does not grant device configuration rights"
        );
        let after = app
            .call("inspect-display-configuration", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn readonly_display_surface_keeps_inspection_and_has_no_stale_admin_controls() {
        for role in [ui::Role::Viewer, ui::Role::Editor] {
            let mut app =
                ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(role).unwrap();
            let open = crate::categories::Category::Displays;
            app.effect_on(
                &crate::effects::pane_row_id(open),
                &crate::effects::open_pane(open),
            )
            .unwrap();
            app.settle();
            let audit = app.harness().native_app_audit();
            assert!(
                audit
                    .issues
                    .iter()
                    .all(|issue| issue.code != "stale-action-binding"),
                "role {role:?}: stale control in the actual application and host doors: {:?}",
                audit.issues
            );
            assert!(
                app.call("inspect-display-configuration", serde_json::json!({}))
                    .unwrap()
                    .is_some()
            );
            for action in [
                "identify-displays",
                "apply-display-changes",
                "keep-display-changes",
                "revert-display-changes",
            ] {
                assert!(
                    app.call(action, serde_json::json!({})).is_err(),
                    "{role:?} cannot invoke {action}"
                );
            }
        }
    }

    #[test]
    fn readonly_host_reply_replaces_stale_observation_without_staging_or_disk_writes() {
        let mut model = model(vec![classify_connector("eDP-1", "connected")]);
        let mode = OutputMode {
            width: 2944,
            height: 1840,
            refresh_mhz: 89999,
        };
        let observed = DisplayHardwareSnapshot {
            outputs: vec![settings_store::hardware::DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: "eDP-1".into(),
                    connector_label: "eDP-1".into(),
                    modes: vec![mode],
                    hdr: false,
                    vrr: false,
                    color_profiles: vec![],
                    monitor: None,
                },
                current_mode: mode,
                scale: 2.0,
                builtin: true,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            }],
            ..DisplayHardwareSnapshot::default()
        };
        model.accept_host_reply(settings_store::display_control::DisplayReply {
            identify: None,
            observed,
            transaction: None,
            committed: DisplayConfig::default(),
        });
        let inspection = model.inspection();
        assert_eq!(
            inspection["observed_outputs"][0]["current_mode"]["refresh_mhz"],
            89999
        );
        assert_eq!(inspection["observed_outputs"][0]["scale"], 2.0);
        assert_eq!(inspection["has_staged_changes"], false);
        assert!(inspection["transaction"].is_null());
        assert!(model.pending_candidate.is_none());
    }

    fn model(outputs: Vec<DisplayOutput>) -> DisplayModel {
        let profile = tempfile::tempdir().unwrap().keep();
        DisplayModel::detached(PaneCx::in_file(profile.join("settings.json"))).with_outputs(outputs)
    }

    /// An output named after the DRM connector `conn` (`card0-eDP-1`, with the
    /// `cardN-` prefix dropped); eDP/LVDS/DSI panels are built in.
    fn classify_connector(conn: &str, status: &str) -> DisplayOutput {
        // Strip the leading `cardN-` so we're left with the connector, e.g.
        // `eDP-1`, `HDMI-A-1`, `DP-2`.
        let name = conn.split_once('-').map_or(conn, |(_, rest)| rest);
        let upper = name.to_ascii_uppercase();
        let builtin =
            upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
        let label = if builtin {
            "Built-in display".to_string()
        } else {
            name.to_string()
        };
        DisplayOutput {
            label,
            connected: status.trim() == "connected",
            builtin,
            connector: name.to_string(),
            connector_label: name.to_string(),
            modes: Vec::new(),
            current_mode: None,
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
        }
    }

    fn recording(outputs: Vec<DisplayOutput>) -> (DisplayModel, Arc<RecordingDispatcher>) {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut model = model(outputs);
        model.brightness_control = Arc::new(SystemBrightness(
            Arc::clone(&rec) as Arc<dyn Dispatcher + Send + Sync>
        ));
        (model, rec)
    }

    fn panel() -> DisplayOutput {
        display_with_modes(
            "eDP-1",
            "Built-in display",
            true,
            vec![OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }],
        )
    }

    /// A shown pane reads the compositor's display snapshot when its watch
    /// says it changed, never on a timer; a pane whose snapshots cannot all
    /// be watched, or whose change awaits confirmation, looks each second.
    #[test]
    fn followed_displays_are_read_on_a_change_not_on_a_timer() {
        let _config = crate::prefs::test_env::ConfigGuard::new();
        let runtime = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let snapshot = settings_store::hardware::display_snapshot_path().unwrap();
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        let long_ago = || ui::time::now().checked_sub(Duration::from_secs(5)).unwrap();
        let mut pane = model(vec![panel()]);
        pane.of_device = true;
        let watched = Rc::new(crate::watch::Watched::by_hand());
        pane.following = Rc::downgrade(&watched);

        std::fs::write(&snapshot, "{}").unwrap();
        pane.last_snapshot_check = long_ago();
        assert!(
            pane.poll(),
            "the first look reads the snapshot that appeared"
        );
        assert_eq!(pane.next_wake(), None, "an idle pane sets no deadline");

        let read = pane.snapshot_stamp;
        assert!(read.is_some());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&snapshot, "{\"moved\":1}").unwrap();
        pane.last_snapshot_check = long_ago();
        assert!(!pane.poll(), "nothing is read without the watch's word");
        assert_eq!(pane.snapshot_stamp, read);

        watched.touch();
        assert!(pane.poll(), "the change is read once the watch reports it");
        assert_ne!(pane.snapshot_stamp, read);
        assert!(!pane.poll(), "and only once");

        pane.settling = true;
        assert!(
            pane.next_wake().is_some(),
            "a change awaiting confirmation counts down each second"
        );
        pane.settling = false;

        let unwatchable = Rc::new(crate::watch::Watched::files(&[runtime
            .path()
            .join("absent")
            .join("display-state.json")]));
        pane.following = Rc::downgrade(&unwatchable);
        assert!(
            pane.next_wake().is_some(),
            "snapshots that cannot be watched are read on the old timer"
        );
        drop(unwatchable);
        assert!(
            pane.next_wake().is_some(),
            "a pane nobody shows keeps its timer for reads an effect asks for"
        );
    }

    /// A brightness that could not be read is shown as that failure, not as
    /// a slider resting at a made-up level; a probe that lands asks for a
    /// rebuild once.
    #[test]
    fn the_brightness_probe_lands_once_and_a_failure_is_shown() {
        let mut model = model(vec![panel()]);
        let (probe_tx, probe_rx) = crate::channel::channel();
        model.hardware_probe = Some(probe_rx);
        probe_tx
            .send((Err("no backlight device".into()), false))
            .expect("probe receiver is alive");
        assert!(model.poll(), "the landed probe requests a rebuild");
        assert!(!model.poll(), "and reports it once");
        let text = crate::widgets::announced_text(model.build_composition().as_ref());
        assert!(
            text.iter()
                .any(|line| line == "Can't read the brightness: no backlight device"),
            "{text:#?}"
        );

        let mut model = self::model(vec![panel()]);
        let (probe_tx, probe_rx) = crate::channel::channel();
        model.hardware_probe = Some(probe_rx);
        probe_tx.send((Ok(0.8), true)).unwrap();
        assert!(model.poll());
        assert!((model.brightness - 0.8).abs() < f32::EPSILON);
        assert!(model.ambient_sensor_available);
    }

    #[test]
    fn sdr_brightness_slider_round_trips_whole_nits_and_reasons_have_copy() {
        use settings_store::hardware::{AdaptiveSyncUnavailable, HdrUnavailable};
        for nits in [80, 203, 300, 480] {
            assert_eq!(sdr_brightness_nits(sdr_brightness_fraction(nits)), nits);
        }
        assert!((sdr_brightness_fraction(80) - 0.0).abs() < f64::EPSILON);
        assert!((sdr_brightness_fraction(480) - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            sdr_brightness_nits(f64::NAN),
            80,
            "garbage lands on the dim end"
        );
        assert_eq!(sdr_brightness_nits(7.0), 480);
        let keys = [
            hdr_unavailable_key(false, None),
            hdr_unavailable_key(true, None),
            hdr_unavailable_key(true, Some(HdrUnavailable::DriverUnsupported)),
            hdr_unavailable_key(true, Some(HdrUnavailable::FormatUnsupported)),
            hdr_unavailable_key(true, Some(HdrUnavailable::DriverRejected)),
            adaptive_sync_unavailable_key(false, None),
            adaptive_sync_unavailable_key(true, Some(AdaptiveSyncUnavailable::DriverUnsupported)),
            adaptive_sync_unavailable_key(true, Some(AdaptiveSyncUnavailable::DriverRejected)),
        ];
        for key in keys {
            let copy = crate::i18n::t(key);
            assert_ne!(copy, key, "{key} has no en-US copy");
        }
    }

    #[test]
    fn relative_positions_use_the_neighbour_s_logical_extent() {
        let mode = |width, height| {
            Some(OutputMode {
                width,
                height,
                refresh_mhz: 60_000,
            })
        };
        let panel = DisplayOutput {
            current_mode: mode(2560, 1600),
            ..classify_connector("card0-eDP-1", "connected")
        };
        let monitor = DisplayOutput {
            current_mode: mode(1920, 1080),
            ..classify_connector("card0-HDMI-A-1", "connected")
        };
        let connected = [panel, monitor];
        let mut next = DisplayConfig::default();
        next.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                scale: 2.0,
                ..Default::default()
            },
        );
        let place = |label: &str, cfg: &mut OutputConfig| {
            place_beside("HDMI-A-1", label, cfg, &next, &connected)
        };

        // The 2x panel is 1280 logical pixels wide, so its right-hand
        // neighbour starts there — not at its 2560-pixel mode width.
        let mut cfg = OutputConfig::default();
        place("Right of Built-in display", &mut cfg).unwrap();
        assert_eq!((cfg.x, cfg.y), (1280, 0));
        place("Below of Built-in display", &mut cfg).unwrap();
        assert_eq!((cfg.x, cfg.y), (0, 800));

        // A monitor rotated to portrait is 1080 wide on the left.
        let mut portrait = OutputConfig {
            transform: OutputTransform::Rotate90,
            ..Default::default()
        };
        place("Left of Built-in display", &mut portrait).unwrap();
        assert_eq!((portrait.x, portrait.y), (-1080, 0));
        assert!(place("Left of Nowhere", &mut portrait).is_err());
    }

    /// Distinct duplicate labels, staging per connector and virtual
    /// connector filtering.
    #[test]
    fn display_connectors_are_labelled_and_filtered() {
        // Case `qualified_duplicate_snapshot_labels_are_distinct_in_settings`.
        {
            let first = OutputDescriptor {
                connector: "pci:0000:01:00.0/HDMI-A-1".into(),
                connector_label: "HDMI-A-1".into(),
                modes: vec![],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            };
            let second = OutputDescriptor {
                connector: "pci:0000:03:00.0/HDMI-A-1".into(),
                connector_label: "HDMI-A-1".into(),
                modes: vec![],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            };
            assert_eq!(
                snapshot_label(&first, false),
                "HDMI-A-1 on pci:0000:01:00.0"
            );
            let named = OutputDescriptor {
                monitor: Some(MonitorIdentity {
                    make: "Dell".into(),
                    model: "DELL U2720Q".into(),
                    ..MonitorIdentity::default()
                }),
                ..first.clone()
            };
            assert_eq!(
                snapshot_label(&named, false),
                "Dell U2720Q on HDMI-A-1 on pci:0000:01:00.0",
                "a monitor is named by its EDID and still told apart by port"
            );
            assert_eq!(
                snapshot_label(&second, false),
                "HDMI-A-1 on pci:0000:03:00.0"
            );

            let display = DisplayOutput {
                label: snapshot_label(&first, false),
                connector: first.connector.clone(),
                connector_label: first.label().to_string(),
                ..classify_connector("card0-HDMI-A-1", "connected")
            };
            let staged = DisplayModel::output_descriptor(&display);
            assert_eq!(staged.connector, "pci:0000:01:00.0/HDMI-A-1");
            assert_eq!(
                staged.connector_label, "HDMI-A-1",
                "the card qualifier is UI-only, never folded into descriptor data"
            );
        }

        // Case `duplicate_connectors_are_staged_apart_and_keep_the_old_key`.
        {
            let first = "pci:0000:01:00.0/HDMI-A-1";
            let second = "pci:0000:03:00.0/HDMI-A-1";
            let mut pane = model(vec![
                DisplayOutput {
                    label: "HDMI-A-1 on pci:0000:01:00.0".into(),
                    connector: first.into(),
                    ..classify_connector("card0-HDMI-A-1", "connected")
                },
                DisplayOutput {
                    label: "HDMI-A-1 on pci:0000:03:00.0".into(),
                    connector: second.into(),
                    ..classify_connector("card1-HDMI-A-1", "connected")
                },
            ]);
            pane.output_config.outputs.insert(
                "HDMI-A-1".into(),
                OutputConfig {
                    enabled: false,
                    ..Default::default()
                },
            );
            pane.apply(&DisplayEffect::SetPrimaryDisplay {
                display: second.into(),
            })
            .unwrap();
            let staged = pane.pending_candidate.as_ref().unwrap();
            assert!(!staged.outputs["HDMI-A-1"].enabled, "the old key is kept");
            assert!(staged.outputs[second].primary);
            assert!(!staged.outputs[first].primary);
        }

        // Case `virtual_connectors_are_filtered`.
        // The compositor's writeback connector and other virtual connectors are
        // not displays a user plugs into, so they are filtered from the list.
        {
            assert!(is_virtual_connector("card0-Writeback-1"));
            assert!(is_virtual_connector("card0-Virtual-1"));
            assert!(!is_virtual_connector("card0-eDP-1"));
            assert!(!is_virtual_connector("card0-HDMI-A-1"));
            assert!(!is_virtual_connector("card0-DP-2"));
            assert!(!is_virtual_connector("card0-USB-1"));
        }
    }

    /// The panel's slider sets its brightness; an external monitor's sets
    /// that monitor's, and only a monitor that has a control gets one.
    #[test]
    fn brightness_reaches_the_panel_and_each_monitor() {
        let (mut pane, rec) = recording(vec![panel()]);
        pane.apply(&DisplayEffect::SetBrightness { brightness: 0.75 })
            .unwrap();
        assert_eq!(rec.calls(), ["set_brightness(75.00)"]);
        assert!((pane.brightness - 0.75).abs() < f32::EPSILON);

        let level = |connector: &str, level: f32| os_actions::MonitorLevel {
            connector: connector.into(),
            level,
            kind: if connector == "eDP-1" {
                os_actions::BrightnessKind::Backlight
            } else {
                os_actions::BrightnessKind::Ddc
            },
        };
        let (mut pane, rec) = recording(vec![
            display_with_modes("eDP-1", "Built-in display", true, Vec::new()),
            display_with_modes("DP-2", "Dell U2720Q on DP-2", false, Vec::new()),
            display_with_modes("DP-3", "LG 27GL850 on DP-3", false, Vec::new()),
        ]);
        pane.output_brightness =
            output_brightness_from(&pane.outputs, &[level("eDP-1", 0.5), level("DP-2", 0.3)]);
        assert_eq!(
            pane.output_brightness.keys().collect::<Vec<_>>(),
            ["DP-2"],
            "the panel keeps the Brightness card; a monitor without DDC/CI gets no slider"
        );
        let dimmed = dimmed_by_tilo_from(
            &pane.outputs,
            &[os_actions::MonitorLevel {
                connector: "DP-3".into(),
                level: 1.0,
                kind: os_actions::BrightnessKind::Software,
            }],
        );
        assert_eq!(
            dimmed.into_iter().collect::<Vec<_>>(),
            ["DP-3"],
            "a monitor the compositor dims says so beside its slider"
        );
        pane.apply(&DisplayEffect::SetMonitorBrightness {
            display: "DP-2".into(),
            brightness: 0.8,
        })
        .unwrap();
        assert!(
            rec.calls()
                .contains(&"set_display_brightness(DP-2, 80.00)".to_string()),
            "{:?}",
            rec.calls()
        );
        assert!(
            pane.apply(&DisplayEffect::SetMonitorBrightness {
                display: "DP-3".into(),
                brightness: 0.8,
            })
            .is_err()
        );
    }

    /// Mirroring picks a shared mode; extending again keeps the origin.
    #[test]
    fn mirroring_and_extending_are_staged() {
        let full_hd_60 = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let full_hd_5994 = OutputMode {
            refresh_mhz: 59_940,
            ..full_hd_60
        };
        let panel = display_with_modes("eDP-1", "Built-in display", true, vec![full_hd_60]);
        let usb_c = display_with_modes("DP-1", "USB-C display", false, vec![full_hd_5994]);
        assert_eq!(
            best_mirror_modes(&usb_c, &panel),
            Some((full_hd_5994, full_hd_60)),
            "software mirroring needs a shared extent, not identical connector clocks"
        );

        let mut pane = model(vec![panel, usb_c]);
        pane.output_config.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                mode: Some(full_hd_60),
                ..Default::default()
            },
        );
        pane.output_config.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                x: 1920,
                mode: Some(full_hd_5994),
                ..Default::default()
            },
        );
        pane.apply(&DisplayEffect::MirrorDisplay {
            display: "DP-1".into(),
            mirror: "Mirror Built-in display".into(),
        })
        .unwrap();
        let mirrored = pane.pending_candidate.clone().expect("mirror plan staged");
        assert_eq!(mirrored.outputs["DP-1"].mirror_of.as_deref(), Some("eDP-1"));
        assert_eq!(mirrored.outputs["DP-1"].mode, Some(full_hd_5994));
        assert_eq!(mirrored.outputs["eDP-1"].mode, Some(full_hd_60));

        pane.output_config = mirrored;
        pane.pending_candidate = None;
        pane.apply(&DisplayEffect::MirrorDisplay {
            display: "DP-1".into(),
            mirror: "Extend workspace".into(),
        })
        .unwrap();
        let extended = pane.pending_candidate.as_ref().expect("extend plan staged");
        assert_eq!(extended.outputs["DP-1"].mirror_of, None);
        assert_eq!(
            (extended.outputs["DP-1"].x, extended.outputs["DP-1"].y),
            (1920, 0)
        );

        let shown = model(vec![
            self::panel(),
            DisplayOutput {
                connected: false,
                ..display_with_modes("DP-2", "DP-2", false, Vec::new())
            },
        ]);
        assert_eq!(
            shown.connected_outputs().len(),
            1,
            "idle ports are not listed"
        );
    }

    fn display_with_modes(
        connector: &str,
        label: &str,
        builtin: bool,
        modes: Vec<OutputMode>,
    ) -> DisplayOutput {
        DisplayOutput {
            label: label.into(),
            connected: true,
            builtin,
            connector: connector.into(),
            connector_label: connector.into(),
            current_mode: modes.first().copied(),
            modes,
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
        }
    }

    #[test]
    fn preview_and_keep_preserve_the_staged_refresh_rate() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let current = OutputMode {
            width: 2944,
            height: 1840,
            refresh_mhz: 59_999,
        };
        let requested = OutputMode {
            refresh_mhz: 89_999,
            ..current
        };
        let mut pane = model(vec![display_with_modes(
            "eDP-1",
            "Built-in display",
            true,
            vec![current, requested],
        )]);
        pane.apply(&DisplayEffect::SetDisplayMode {
            display: "eDP-1".into(),
            mode: requested.label(),
        })
        .unwrap();
        pane.apply(&DisplayEffect::ApplyDisplayChanges).unwrap();
        let mut transaction = DisplayTransaction::load().expect("Apply requests a preview");
        assert_eq!(transaction.state, DisplayTransactionState::Requested);
        assert_eq!(transaction.candidate.outputs["eDP-1"].mode, Some(requested));
        let preview_id = transaction.id;
        pane.settle(&transaction);
        assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));

        transaction.state = DisplayTransactionState::AwaitingConfirmation;
        transaction.deadline_unix_ms = Some(unix_now_ms() + 15_000);
        transaction.save().unwrap();
        pane.settle(&transaction);
        pane.apply(&DisplayEffect::KeepDisplayChanges).unwrap();
        let mut kept = DisplayTransaction::load().unwrap();
        assert_eq!(kept.id, preview_id, "Keep confirms the original preview");
        assert_eq!(kept.state, DisplayTransactionState::ConfirmRequested);
        assert_eq!(kept.candidate.outputs["eDP-1"].mode, Some(requested));
        pane.settle(&kept);
        assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));

        // The compositor commits the kept arrangement before acknowledging it.
        kept.candidate.save_for(&kept.set).unwrap();
        kept.state = DisplayTransactionState::Confirmed;
        kept.deadline_unix_ms = None;
        kept.save().unwrap();
        pane.settle(&DisplayTransaction::load().unwrap());
        assert!(pane.pending_candidate.is_none());
        assert_eq!(pane.staged().outputs["eDP-1"].mode, Some(requested));
    }

    /// Scale labels, the person's display scale, and a staged plan that is
    /// applied, kept and reverted.
    #[test]
    fn display_scale_and_plans_persist_confirm_and_revert() {
        for (label, factor) in DISPLAY_SCALES {
            assert_eq!(scale_label(*factor), *label);
            assert_eq!(scale_factor_of(label), Some(*factor));
        }
        assert_eq!(scale_label(1.48), "150%");
        assert_eq!(scale_factor_of("bogus"), None);

        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = model(vec![panel()]);
        pane.cx
            .settings::<DisplayPrefs>()
            .update(|prefs| prefs.scale = 1.5)
            .unwrap();
        assert!((pane.cx.settings::<DisplayPrefs>().get().scale - 1.5).abs() < 1e-9);

        let mut committed = DisplayConfig::default();
        committed.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                mode: pane.outputs[0].current_mode,
                ..Default::default()
            },
        );
        let set = pane.display_set();
        committed.save_for(&set).unwrap();
        pane.output_config = committed.clone();
        pane.apply(&DisplayEffect::RotateDisplay {
            display: "eDP-1".into(),
            rotation: "Portrait right".into(),
        })
        .unwrap();
        assert_eq!(
            pane.pending_candidate.as_ref().unwrap().outputs["eDP-1"].transform,
            OutputTransform::Rotate90
        );
        assert_eq!(
            DisplayConfig::load_for(&set),
            committed,
            "editing is only staged"
        );

        pane.apply(&DisplayEffect::ApplyDisplayChanges).unwrap();
        let mut transaction = DisplayTransaction::load().expect("Apply wrote a transaction");
        assert_eq!(transaction.state, DisplayTransactionState::Requested);
        assert_eq!(
            transaction.set, set,
            "the plan is for the monitors it was arranged on"
        );
        transaction.state = DisplayTransactionState::AwaitingConfirmation;
        transaction.deadline_unix_ms = Some(unix_now_ms() + 15_000);
        transaction.save().unwrap();
        pane.apply(&DisplayEffect::KeepDisplayChanges).unwrap();
        assert_eq!(
            DisplayTransaction::load().unwrap().state,
            DisplayTransactionState::ConfirmRequested
        );

        transaction.state = DisplayTransactionState::Reverted;
        transaction.deadline_unix_ms = None;
        transaction.save().unwrap();
        pane.settle(&DisplayTransaction::load().unwrap());
        assert!(pane.pending_candidate.is_none());
        assert_eq!(
            pane.staged().outputs["eDP-1"].transform,
            OutputTransform::Normal,
            "revert shows the last known-good arrangement, not the failed plan"
        );
    }

    fn laptop_with_monitor() -> DisplayModel {
        let full_hd = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let qhd = OutputMode {
            width: 2560,
            height: 1440,
            refresh_mhz: 60_000,
        };
        let mut pane = model(vec![
            display_with_modes("HDMI-A-1", "Desk monitor", false, vec![qhd]),
            display_with_modes("eDP-1", "Built-in display", true, vec![full_hd]),
        ]);
        pane.output_config.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                ..Default::default()
            },
        );
        pane.output_config.outputs.insert(
            "HDMI-A-1".into(),
            OutputConfig {
                x: 1920,
                ..Default::default()
            },
        );
        pane
    }

    #[test]
    fn the_arrangement_numbers_and_sizes_displays_like_identify() {
        let pane = laptop_with_monitor();
        let connected = pane.connected_outputs();
        let displays = pane.arranged_displays(&connected);
        let panel = displays.iter().find(|d| d.connector == "eDP-1").unwrap();
        let monitor = displays.iter().find(|d| d.connector == "HDMI-A-1").unwrap();
        assert_eq!((panel.number, monitor.number), (1, 2));
        assert!(panel.primary && !monitor.primary);
        assert_eq!(
            monitor.placement,
            super::super::display_arrangement::placement::Placement::new(1920, 0, 2560, 1440)
        );
    }

    #[test]
    fn the_arrangement_draws_mixed_dpi_displays_at_their_logical_size() {
        use super::super::display_arrangement::placement::{Placement, settle};
        // A 2x panel of 2560x1600 is 1280x800 on the desktop the pointer
        // moves through; a 1x monitor beside it starts at x = 1280.
        let panel_mode = OutputMode {
            width: 2560,
            height: 1600,
            refresh_mhz: 60_000,
        };
        let monitor_mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let mut pane = model(vec![
            display_with_modes("eDP-1", "Built-in display", true, vec![panel_mode]),
            display_with_modes("HDMI-A-1", "Desk monitor", false, vec![monitor_mode]),
        ]);
        pane.output_config.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                scale: 2.0,
                ..Default::default()
            },
        );
        pane.output_config.outputs.insert(
            "HDMI-A-1".into(),
            OutputConfig {
                x: 1280,
                ..Default::default()
            },
        );
        let connected = pane.connected_outputs();
        let displays = pane.arranged_displays(&connected);
        let panel = displays.iter().find(|d| d.connector == "eDP-1").unwrap();
        let monitor = displays.iter().find(|d| d.connector == "HDMI-A-1").unwrap();
        assert_eq!(panel.placement, Placement::new(0, 0, 1280, 800));
        assert_eq!(monitor.placement, Placement::new(1280, 0, 1920, 1080));
        assert!(
            panel.placement.touches(monitor.placement),
            "the canvas shows the edge the pointer crosses"
        );
        // Dropping the monitor near the panel's right edge snaps it onto
        // that logical edge, never onto the panel's mode width.
        let dropped = settle(monitor.placement.at(1300, 12), &[panel.placement], 24);
        assert_eq!((dropped.x, dropped.y), (1280, 0));
    }

    #[test]
    fn an_unconfigured_display_is_drawn_and_staged_at_the_compositors_scale() {
        // Nothing saved: the compositor runs the HiDPI panel at the 2x it
        // chose itself, so the canvas draws it 1280 wide and staging a
        // change keeps that scale rather than writing 1x.
        let mut panel = display_with_modes(
            "eDP-1",
            "Built-in display",
            true,
            vec![OutputMode {
                width: 2560,
                height: 1600,
                refresh_mhz: 60_000,
            }],
        );
        panel.scale = 2.0;
        let mut pane = model(vec![panel]);
        let connected = pane.connected_outputs();
        let displays = pane.arranged_displays(&connected);
        assert_eq!(displays[0].placement.width, 1280);
        assert_eq!(displays[0].placement.height, 800);
        pane.apply(&DisplayEffect::RotateDisplay {
            display: "eDP-1".into(),
            rotation: "Landscape".into(),
        })
        .unwrap();
        assert!(
            (pane.staged().outputs["eDP-1"].scale - 2.0).abs() < f64::EPSILON,
            "staging keeps the scale the display runs at"
        );
    }

    #[test]
    fn a_dropped_display_stages_its_exact_position() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        pane.apply(&DisplayEffect::MoveDisplay {
            display: "HDMI-A-1".into(),
            x: -2560,
            y: -360,
        })
        .unwrap();
        let staged = pane.pending_candidate.as_ref().expect("the drop is staged");
        assert_eq!(
            (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
            (-2560, -360)
        );
        assert!(
            DisplayTransaction::load().is_none(),
            "a drop is staged like every other edit, not applied"
        );
        pane.apply(&DisplayEffect::PlaceDisplay {
            display: "HDMI-A-1".into(),
            position: "Right of Built-in display".into(),
        })
        .unwrap();
        let staged = pane.pending_candidate.as_ref().unwrap();
        assert_eq!(
            (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
            (1920, 0)
        );
    }

    #[test]
    fn identify_asks_the_compositor_to_number_the_displays() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        assert!(settings_store::display_identify::IdentifyRequest::load().is_none());
        let before = unix_now_ms();
        pane.apply(&DisplayEffect::IdentifyDisplays).unwrap();
        let after = unix_now_ms();
        let request = settings_store::display_identify::IdentifyRequest::load()
            .expect("Identify writes the request the compositor polls");
        assert!((before..=after).contains(&request.requested_unix_ms));
        assert_eq!(
            request.remaining_ms(request.requested_unix_ms),
            Some(settings_store::display_identify::IDENTIFY_MS)
        );
    }

    #[test]
    fn a_trusted_display_tool_is_listed_and_can_be_asked_again() {
        use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        pane.of_device = true;
        assert!(pane.build_trusted_tools_card().is_none());
        let kanshi = DisplayTool::new("/usr/bin/kanshi");
        TrustedDisplayTools::trust(kanshi.clone()).expect("trust");
        assert!(pane.build_trusted_tools_card().is_some());
        pane.apply(&DisplayEffect::ForgetDisplayTool {
            tool: kanshi.executable.clone(),
        })
        .unwrap();
        assert!(!TrustedDisplayTools::load().contains(&kanshi));
        assert!(pane.build_trusted_tools_card().is_none());
    }
    #[test]
    fn peer_displays_are_inspectable_accessible_and_not_an_input_grant() {
        fn accessible_names(widget: &dyn Widget, names: &mut Vec<String>) {
            fn collect(node: &ui::a11y::AccessNode, names: &mut Vec<String>) {
                if let Some(name) = &node.name {
                    names.push(name.clone());
                }
                for child in &node.children {
                    collect(child, names);
                }
            }
            if let Some(node) = widget.accessibility() {
                collect(&node, names);
            }
            for child in widget.children() {
                accessible_names(child, names);
            }
        }
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor();
        assert!(pane.build_device_arrangement_card().is_none());
        pane.device_displays = vec![settings_store::device_displays::DeviceDisplay {
            device: "certified-peer".into(),
            device_label: "Peer laptop".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: false,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 1,
        }];
        let card = pane
            .build_device_arrangement_card()
            .expect("discovered peer has a real canvas");
        let mut names = Vec::new();
        let arrangement = crate::widgets::page(vec![card]);
        accessible_names(arrangement.as_ref(), &mut names);
        assert!(
            names
                .iter()
                .any(|name| name.contains("Peer laptop") && name.contains("Offline")),
            "assistive users receive actual peer and offline state"
        );
        let inspection = pane.inspection();
        assert_eq!(inspection["device_displays"][0]["device"], "certified-peer");
        assert_eq!(inspection["device_displays"][0]["scale_milli"], 1250);
        assert!(
            pane.apply(&DisplayEffect::MoveDisplay {
                display: "certified-peer/eDP-1".into(),
                x: 0,
                y: 0,
            })
            .is_err(),
            "read-only discovery cannot edit local outputs or grant input"
        );
    }
}
