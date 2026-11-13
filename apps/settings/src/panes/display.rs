//! Display: the connected outputs and their arrangement, brightness and the
//! display comfort preferences.
//!
//! The compositor owns KMS, so Settings takes modes and capabilities from its
//! snapshot and never guesses them from sysfs.

mod page;
pub(crate) mod shared_displays;

pub(crate) use page::view;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_clock::unix_now_ms;
use ui::time::{Duration, Instant, SystemTime};

use os_actions::Dispatcher;
use provider_settings_ui::bound::SettingRows as _;
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::widget::{Dropdown, Slider, Toggle};

use crate::area::AreaContext;
use crate::dispatcher::dispatcher_or_real;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::{DisplayPrefs, NightLightPrefs, PendingDisplayChange};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, note, rows, section, setting_row, setting_row_described,
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
    fn busy(&self) -> bool {
        self.model.get().is_some_and(|model| model.borrow().busy())
    }
    fn next_wake(&self) -> Option<Instant> {
        self.model.get().and_then(|model| model.borrow().next_wake())
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&DisplayArea> {
    app.areas().find()
}

pub(crate) mod values;

pub(crate) fn night_light(cx: &ViewCx<NightLightPrefs>) -> Item {
    cx.setting(t("display-night-light"))
        .id("display.night_light")
        .toggle(NightLightPrefs::paths().night_light)
}

pub(crate) fn automatic_brightness(cx: &ViewCx<DisplayPrefs>) -> Item {
    cx.setting(t("display-auto-brightness"))
        .id("display.auto_brightness")
        .toggle_verb(
            |prefs: &DisplayPrefs| prefs.auto_brightness,
            DisplayPrefs::SET_AUTOMATIC_BRIGHTNESS,
        )
}

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DisplayEffect {
    /// The built-in panel's brightness, from 0 to 1.
    #[effect(returns, idempotent)]
    SetBrightness { brightness: f64 },
    /// An external monitor's brightness, from 0 to 1.
    #[effect(returns, row, idempotent)]
    SetMonitorBrightness { display: String, brightness: f64 },
    /// Use a display, or turn it off. Staged until applied.
    #[effect(returns, row, idempotent)]
    UseDisplay { display: String, on: bool },
    /// Make a display the one panels and new windows open on.
    #[effect(returns, row, idempotent)]
    SetPrimaryDisplay { display: String },
    /// A display's scale. Staged until applied.
    #[effect(returns, row, idempotent)]
    #[effect(keywords = "scaling, zoom, text size, dpi")]
    SetOutputScale { display: String, scale: String },
    /// Place a display beside another: `Right of …`, `Left of …`, `Above …`,
    /// `Below …`. Staged until applied.
    #[effect(returns, row)]
    PlaceDisplay { display: String, position: String },
    /// Put a display's top-left corner at a point of the desktop, as
    /// dragging it does. Staged until applied.
    #[effect(returns, row)]
    MoveDisplay { display: String, x: i32, y: i32 },
    /// Place a device's screens in the person's shared layout by where one of them goes: where
    /// they meet another device's screens is where the pointer crosses between the two.
    #[effect(returns, row)]
    MoveDeviceDisplay {
        display: String,
        revision: u64,
        x: i32,
        y: i32,
    },
    /// Inspect the observed displays and their shared arrangement, including offline devices.
    #[effect(view, returns, kind = "read")]
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
    #[effect(keywords = "resolution, refresh rate, hz, screen size")]
    SetDisplayMode { display: String, mode: String },
    /// A display's rotation. Staged until applied.
    #[effect(returns, row, idempotent)]
    #[effect(keywords = "rotate screen, portrait, landscape, orientation")]
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
    #[effect(view, returns, kind = "read")]
    InspectDisplayConfiguration,
    /// Show each display's number and name on it for a few seconds.
    #[effect(returns)]
    IdentifyDisplays,
    /// Preview the staged arrangement; it is kept only when confirmed.
    #[effect(returns)]
    ApplyDisplayChanges,
    /// Keep the arrangement being previewed.
    #[effect(returns)]
    KeepDisplayChanges,
    /// Go back to the arrangement before the preview.
    #[effect(returns)]
    RevertDisplayChanges,
    /// Ask again before keeping display changes from a layout tool.
    #[effect(returns, row, label = "display-tools-forget")]
    ForgetDisplayTool {
        #[param("The tool's executable")]
        tool: String,
    },
}

const STAGED: &str = "Display change staged. Apply it to preview the new configuration.";

#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DisplayEffect,
    cx: &mut EffectCx<Settings>,
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
        return Ok(app.job(
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

pub const DISPLAY_SCALES: &[(&str, f64)] = &[
    ("100%", 1.0),
    ("125%", 1.25),
    ("150%", 1.5),
    ("175%", 1.75),
    ("200%", 2.0),
];

pub(crate) fn scale_label(scale: f64) -> &'static str {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited setting
    // could hold a non-finite scale, and this must degrade to "100%" rather
    // than panic the pane.
    DISPLAY_SCALES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("100%", |(label, _)| *label)
}

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

fn resolution_label(mode: OutputMode) -> String {
    resolution_text(mode.width, mode.height)
}

fn resolution_text(width: u32, height: u32) -> String {
    // ratchet-allow(glyph_icon): the multiplication sign of a resolution is text, not an icon
    format!("{width} × {height}")
}

fn refresh_label(mode: OutputMode) -> String {
    let label = mode.label();
    label
        .split_once(" at ")
        .map_or_else(|| label.clone(), |(_, rate)| rate.to_owned())
}

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

const SAMPLE_MONITOR: &str = "DP-1";

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

#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors an independent DRM/compositor capability flag (connected, \
              builtin, hdr, sink_hdr, vrr, sink_vrr); they are not related toggles that could \
              collapse into an enum"
)]
pub struct DisplayOutput {
    pub label: String,
    pub connected: bool,
    pub builtin: bool,
    pub connector: String,
    /// Raw KMS connector label, retained separately from [`Self::label`]
    /// because the UI appends a card qualifier to duplicate labels.
    pub connector_label: String,
    pub modes: Vec<OutputMode>,
    pub current_mode: Option<OutputMode>,
    pub scale: f64,
    pub hdr: bool,
    pub sink_hdr: bool,
    pub hdr_unavailable_reason: Option<settings_store::hardware::HdrUnavailable>,
    pub vrr: bool,
    pub sink_vrr: bool,
    pub vrr_unavailable_reason: Option<settings_store::hardware::AdaptiveSyncUnavailable>,
    pub unavailable_reason: Option<settings_store::hardware::OutputUnavailable>,
    pub color_profiles: Vec<String>,
    pub color_profile_labels: BTreeMap<String, String>,
    pub monitor: Option<MonitorIdentity>,
}

fn output_unavailable_key(reason: settings_store::hardware::OutputUnavailable) -> &'static str {
    use settings_store::hardware::OutputUnavailable;
    match reason {
        OutputUnavailable::LinkDown => "display-link-down",
        OutputUnavailable::DriverRejected => "display-output-driver-rejected",
    }
}

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

fn sdr_brightness_fraction(nits: u16) -> f64 {
    let range = settings_store::hardware::SDR_BRIGHTNESS_NITS_RANGE;
    let (low, high) = (f64::from(*range.start()), f64::from(*range.end()));
    ((f64::from(nits) - low) / (high - low)).clamp(0.0, 1.0)
}

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
    fn unconfigured(&self) -> OutputConfig {
        OutputConfig::at_scale(self.scale)
    }
}

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
    out.sort_by(|a, b| {
        b.builtin
            .cmp(&a.builtin)
            .then(b.connected.cmp(&a.connected))
            .then(a.label.cmp(&b.label))
    });
    out
}

fn read_output_brightness(outputs: &[DisplayOutput]) -> BTreeMap<String, f32> {
    if !outputs
        .iter()
        .any(|output| output.connected && !output.builtin)
    {
        return BTreeMap::new();
    }
    output_brightness_from(outputs, &os_actions::BrightnessService::global().levels())
}

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

pub trait BrightnessControl: Send + Sync {
    fn set_brightness(&self, percent: f32) -> Result<(), String>;
    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String>;
}

struct SystemBrightness(Arc<dyn Dispatcher + Send + Sync>);

impl BrightnessControl for SystemBrightness {
    fn set_brightness(&self, percent: f32) -> Result<(), String> {
        self.0.set_brightness(percent)
    }
    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
        self.0.set_display_brightness(connector, percent)
    }
}

struct NoBrightness;

impl BrightnessControl for NoBrightness {
    fn set_brightness(&self, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_display_brightness(&self, _: &str, _: f32) -> Result<(), String> {
        Ok(())
    }
}

type HardwareProbe = (Result<f32, String>, bool);

pub type SharedModel = Rc<std::cell::RefCell<DisplayModel>>;

pub struct DisplayArea {
    devices: bool,
    storage: ui::storage::AppStorage,
    sections: crate::sections::Sections,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    model: std::cell::OnceCell<SharedModel>,
    shown: LocalState<u64>,
}

impl DisplayArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            devices: cx.devices,
            storage: cx.storage.clone(),
            sections: cx.sections.clone(),
            scene: cx.scene.clone(),
            model: std::cell::OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    pub fn model(&self) -> &SharedModel {
        self.model.get_or_init(|| {
            let (storage, sections) = (self.storage.clone(), self.sections.clone());
            let model = if self.devices {
                DisplayModel::of_device(storage, sections)
            } else {
                DisplayModel::sample(storage, sections, self.scene.as_deref())
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    pub(crate) fn read(&self) -> Option<&SharedModel> {
        self.model.get()
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && model.borrow_mut().poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let mut rows: Vec<LiveRow> = model
            .connected_outputs()
            .into_iter()
            .map(|output| model.output_row(output))
            .collect();
        rows.extend(model.shared_display_rows());
        rows.extend(model.trusted_tools().iter().map(DisplayModel::tool_row));
        rows
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: whose displays, what the hardware offers, what awaits an answer, whether it changed"
)]
pub struct DisplayModel {
    storage: ui::storage::AppStorage,
    sections: crate::sections::Sections,
    of_device: bool,
    brightness_control: Arc<dyn BrightnessControl>,
    outputs: Vec<DisplayOutput>,
    /// Whether the compositor's display snapshot could be read: without
    /// it the pane cannot tell what is connected, which it must not show
    /// as "no displays".
    outputs_read: bool,
    person_devices: Vec<settings_store::person_displays::PersonDevice>,
    pending_change: Option<DisplayTransaction>,
    ambient_sensor_available: bool,
    brightness: f32,
    output_config: DisplayConfig,
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
    following: std::rc::Weak<crate::watch::Watched>,
    settling: bool,
    levels_landed: Arc<AtomicBool>,
    hardware_probe: Option<crate::channel::Receiver<HardwareProbe>>,
    brightness_error: Option<String>,
    brightness_tx: Option<crate::channel::Sender<f32>>,
    brightness_result_rx: Option<crate::channel::Receiver<Result<f32, String>>>,
    pending_brightness: Option<f32>,
    notice: Option<String>,
    arrangement_drag: LocalState<super::display_arrangement::ArrangementGesture>,
    device_arrangement_drag: LocalState<super::display_arrangement::ArrangementGesture>,
    selected: LocalState<String>,
    selected_seen: u64,
    identify_window: Option<(Instant, Instant)>,
    sample: Option<crate::scenarios::DisplaysSample>,
    changed: bool,
}

impl DisplayModel {
    fn tool_row(tool: &settings_store::display_tools::DisplayTool) -> LiveRow {
        LiveRow::new(
            format!("settings.display-tool:{}", tool.executable),
            "settings.display-tool",
            tool.name().to_owned(),
        )
        .operation(
            SettingsEffect::from(DisplayEffect::ForgetDisplayTool {
                tool: tool.executable.clone(),
            })
            .bound(),
        )
    }

    fn of_device(storage: ui::storage::AppStorage, sections: crate::sections::Sections) -> Self {
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
        .map(|error| {
            crate::i18n::t_args(
                "display-brightness-failed",
                &[("detail", ui::i18n::ArgValue::Text(error.to_string()))],
            )
        });
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
            pending_change: None,
            storage,
            sections,
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
            identify_window: None,
            sample: None,
            person_devices: settings_store::person_displays::PersonDisplays::load()
                .map_or_else(Vec::new, |view| view.devices),
            changed: false,
        };
        model.normalize_selection();
        model
    }

    fn sample(
        storage: ui::storage::AppStorage,
        sections: crate::sections::Sections,
        scene: Option<&crate::scenarios::CaptureScene>,
    ) -> Self {
        let displays = scene.and_then(|scene| scene.displays);
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
        let mut model = Self::detached(storage, sections).with_outputs(outputs);
        model.person_devices = scene.map_or_else(Vec::new, |scene| scene.person_devices.clone());
        if let Some(sample) = displays {
            model.stage_sample(sample);
        }
        model
    }

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

    fn detached(storage: ui::storage::AppStorage, sections: crate::sections::Sections) -> Self {
        Self {
            pending_change: None,
            storage,
            sections,
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
            identify_window: None,
            sample: None,
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

    /// Take in a choice made on the canvas: the page shows the chosen
    /// display's settings. Nothing shows on the displays themselves for it:
    /// the canvas tiles carry the numbers, and the cards on the screens come
    /// only when the person presses Identify.
    fn poll_selection(&mut self) {
        let revision = self.selected.revision();
        if revision != self.selected_seen {
            self.selected_seen = revision;
            self.changed = true;
        }
        self.normalize_selection();
    }

    fn identify_shown(&mut self) {
        let now = ui::time::now();
        self.identify_window = Some((
            now,
            now + Duration::from_millis(settings_store::display_identify::IDENTIFY_MS),
        ));
        self.changed = true;
    }

    fn trusted_tools(&self) -> Vec<settings_store::display_tools::DisplayTool> {
        if !self.of_device {
            return Vec::new();
        }
        settings_store::display_tools::TrustedDisplayTools::load().tools
    }

    /// The outputs the arrangement card lists as rows: only the connected
    /// ones. Disconnected physical ports are unused hardware, not displays —
    /// enumerating each one (a laptop's GPU can expose a dozen idle DP
    /// connectors) is noise, so they are summarised in a caption instead.
    fn connected_outputs(&self) -> Vec<&DisplayOutput> {
        self.outputs.iter().filter(|o| o.connected).collect()
    }

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
        self.newest_transaction(
            self.of_device
                .then(|| self.pending_change.clone())
                .flatten(),
        )
    }

    /// The display change being tried as the settings document holds it
    /// now, whether or not a page took it in since, unless the host's last
    /// answer names a newer one: what an agent keeps or reverts while
    /// Displays is closed.
    fn stored_transaction(&self) -> Option<DisplayTransaction> {
        self.newest_transaction(
            self.of_device
                .then(|| {
                    self.storage
                        .settings::<PendingDisplayChange>()
                        .get()
                        .transaction()
                })
                .flatten(),
        )
    }

    fn newest_transaction(&self, shown: Option<DisplayTransaction>) -> Option<DisplayTransaction> {
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
                    .stored_transaction()
                    .ok_or("There is no display preview to keep.")?
                    .id,
            }),
            DisplayEffect::RevertDisplayChanges => Ok(DisplayCommand::Revert {
                transaction: self
                    .stored_transaction()
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
        if reply.identify.is_some() {
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
            "shared_displays": self.shared_display_inspection(),
            "saved_configuration": self.output_config,
            "staged_configuration": self.staged(),
            "has_staged_changes": self.pending_candidate.is_some(),
            "transaction": self.transaction(),
            "notice": self.notice,
        })
    }

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
                choices.push(crate::i18n::t_args(
                    "display-mirror",
                    &[("display", ui::i18n::ArgValue::Text(target.label.clone()))],
                ));
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
        dispatcher: &ui::framework::Dispatcher<Settings>,
        display: &str,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> Option<ui::tilo_actions::ActionBinding<I>> {
        let output = self
            .connected_outputs()
            .into_iter()
            .find(|output| output.connector == display)?;
        crate::on_row::binding(dispatcher, &self.output_row(output).into_resource(), effect)
    }

    fn bind_output<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        dispatcher: &ui::framework::Dispatcher<Settings>,
        display: &str,
        control: C,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        match self.output_binding(dispatcher, display, effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    fn id(display: &str, field: &str) -> String {
        output_key(display, field)
    }

    fn toggle(
        &self,
        dispatcher: &ui::framework::Dispatcher<Settings>,
        display: &str,
        field: &str,
        on: bool,
        set: ActionRef<SettingsEffect, (String, bool)>,
    ) -> Toggle {
        let target = display.to_owned();
        self.bind_output(
            dispatcher,
            display,
            Toggle::from_state(&LocalState::new(on)).with_id(&Self::id(display, field)),
            set.map(move |on: bool| (target.clone(), on)),
        )
    }

    fn slider(
        &self,
        dispatcher: &ui::framework::Dispatcher<Settings>,
        display: &str,
        field: &str,
        value: f64,
        set: ActionRef<SettingsEffect, (String, f64)>,
    ) -> Slider {
        let target = display.to_owned();
        self.bind_output(
            dispatcher,
            display,
            Slider::from_state(&LocalState::new(value))
                .shows_value()
                .with_id(&Self::id(display, field)),
            set.map(move |value: f64| (target.clone(), value)),
        )
    }

    fn choice(
        &self,
        dispatcher: &ui::framework::Dispatcher<Settings>,
        display: &str,
        field: &str,
        choices: Vec<String>,
        current: &str,
        set: ActionRef<SettingsEffect, (String, String)>,
    ) -> Dropdown {
        let target = display.to_owned();
        self.bind_output(
            dispatcher,
            display,
            Dropdown::from_state(choices, &LocalState::new(current.to_owned()))
                .with_id(&Self::id(display, field))
                .with_placeholder(current),
            set.map(move |choice: String| (target.clone(), choice)),
        )
    }

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
            DisplayEffect::IncludeSharedDisplay { .. }
            | DisplayEffect::OrderSharedDisplay { .. } => self.edit_shared_display(effect),
            DisplayEffect::InspectSharedDisplays => Err(t("display-shared-read-only")),
            DisplayEffect::MoveDeviceDisplay {
                display,
                revision,
                x,
                y,
            } => self.move_shared_device(display, *revision, (*x, *y)),
            DisplayEffect::InspectDisplayConfiguration => {
                Err("Display inspection is read-only.".into())
            }
            DisplayEffect::SetBrightness { brightness } => self.set_brightness(*brightness),
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
                    self.notice = Some(crate::i18n::t_args(
                        "display-brightness-failed",
                        &[("detail", ui::i18n::ArgValue::Text(error))],
                    ));
                }
            }
            self.changed = true;
        }
    }

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
    /// When they cannot all be watched, at most once a second.
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

    fn poll_device(&mut self) {
        if !self.of_device || self.following.upgrade().is_none() {
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

    fn settle(&mut self, transaction: &DisplayTransaction) {
        let marker = (transaction.id, transaction.state);
        if self.last_seen_transaction == Some(marker) {
            return;
        }
        self.last_seen_transaction = Some(marker);
        self.changed = true;
        let notice = match transaction.state {
            DisplayTransactionState::Confirmed => Some(t("display-changes-kept")),
            DisplayTransactionState::Reverted => Some(t("display-changes-reverted")),
            DisplayTransactionState::Rejected => None,
            _ => return,
        };
        self.output_config = self.host_state.as_ref().map_or_else(
            || DisplayConfig::load_for(&self.display_set()),
            |state| state.committed.clone(),
        );
        self.pending_candidate = None;
        self.notice = notice;
    }

    fn arrangement_canvas(
        &self,
        dispatcher: &ui::framework::Dispatcher<Settings>,
        connected: &[&DisplayOutput],
    ) -> super::display_arrangement::DisplayArrangement {
        let displays = self.arranged_displays(connected);
        let moves = displays
            .iter()
            .filter(|display| display.movable)
            .filter_map(|display| {
                let connector = display.connector.clone();
                self.output_binding(
                    dispatcher,
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
        .identifying(self.identify_window)
    }

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
}

impl DisplayModel {
    pub(crate) fn poll(&mut self) -> bool {
        self.poll_background();
        self.poll_device();
        self.poll_selection();
        std::mem::take(&mut self.changed)
    }

    pub(crate) fn saw_pending_change(&mut self, change: Option<DisplayTransaction>) {
        if self.pending_change == change {
            return;
        }
        self.pending_change = change;
        self.changed = true;
        if let Some(transaction) = self.transaction() {
            self.settle(&transaction);
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.hardware_probe.is_some() || self.pending_brightness.is_some()
    }

    pub(crate) fn next_wake(&self) -> Option<Instant> {
        let watched = self.following.upgrade()?;
        (self.of_device && (!watched.is_complete() || self.settling))
            .then(|| self.last_snapshot_check + Duration::from_secs(1))
    }

    pub(crate) fn follow(&mut self) -> Option<Rc<dyn std::any::Any>> {
        if !self.of_device {
            return None;
        }
        let files: Vec<std::path::PathBuf> = [
            settings_store::hardware::display_snapshot_path(),
            settings_store::person_displays::path(),
        ]
        .into_iter()
        .flatten()
        .collect();
        // The pending display change is read through the settings document
        // ([`PendingDisplayChange`], recorded by the page), never by
        // watching a file.
        let watched =
            crate::watch::shared(&mut self.following, || crate::watch::Watched::files(&files));
        Some(watched)
    }
}

fn awaits_answer(transaction: &DisplayTransaction) -> bool {
    matches!(
        transaction.state,
        DisplayTransactionState::Requested
            | DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::ConfirmRequested
            | DisplayTransactionState::RevertRequested
    )
}

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
