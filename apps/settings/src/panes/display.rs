#![allow(
    clippy::expect_used,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    reason = "display transactions and their closed static action bindings stay together so rollback and hardware state remain auditable"
)]

//! Display pane — real connected-output list, brightness, and the display
//! comfort preferences.
//!
//! - **Displays** consumes the active compositor's private runtime snapshot.
//!   Settings never independently guesses modes or output capabilities from
//!   sysfs; the process that owns KMS is the authority.
//! - **Brightness** forwards to `os_actions::set_brightness`
//!   (`brightnessctl`), sampled once on construction.
//! - **Automatic brightness / Night light** are durable preferences
//!   persisted to the shared store ([`crate::prefs::SystemPrefs`]); the
//!   compositor's output layer reads them.
//! - **Scale** is the `HiDPI` display-scale factor. Unlike resolution/mode
//!   setting (which stays the compositor's to own), scale is a shared
//!   preference: the compositor reads `display.scale` from the settings
//!   file at session start and advertises it to clients as the preferred
//!   `wp_fractional_scale`, and `ui` apps render crisp at the fractional
//!   factor. That makes it a legitimate, compositor-agnostic control here.
//!
//! Resolution / mode-setting is deliberately *not* offered: there is no
//! unprivileged, compositor-agnostic way to apply a mode from this app, so
//! a picker would list modes it could not switch to. The connector list
//! shows the real hardware instead.
//
// CRDT keys this pane reads / writes:
//   `display.brightness`        — f64 in `[0.0, 1.0]`, slider value
//   `display.auto_brightness`   — bool, durable preference
//   `display.night_light`       — bool, durable preference
//   `display.scale`             — string label ("150%"), durable preference

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tilo_clock::unix_now_ms;
use ui::doc::{read_bool, read_str_or_text};
use ui::time::{Duration, Instant, SystemTime};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Slider, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::dispatcher::dispatcher_or_real;
use crate::i18n::{t, t_args};
use crate::prefs::{DisplayPrefs, SystemPrefs};
use crate::widgets::{
    El, caption, card, choice_row, rows, setting_row, setting_row_described, value_row,
};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplaySet, DisplayTransaction,
    DisplayTransactionState, MonitorIdentity, OutputConfig, OutputDescriptor, OutputMode,
    OutputTransform, logical_extent,
};

/// CRDT key the brightness slider writes into.
pub const BRIGHTNESS_KEY: &str = "display.brightness";
/// CRDT key the auto-brightness toggle writes into.
pub const AUTO_BRIGHTNESS_KEY: &str = "display.auto_brightness";
/// CRDT key the night-light toggle writes into.
pub const NIGHT_LIGHT_KEY: &str = "display.night_light";
pub const NIGHT_LIGHT_SCHEDULE_KEY: &str = "display.night_light_schedule";
pub const NIGHT_LIGHT_TEMPERATURE_KEY: &str = "display.night_light_temperature";
/// CRDT key the display-scale dropdown writes into.
pub const DISPLAY_SCALE_KEY: &str = "display.scale";
pub const APPLY_DISPLAY_KEY: &str = "display.apply";
pub const KEEP_DISPLAY_KEY: &str = "display.keep";
pub const REVERT_DISPLAY_KEY: &str = "display.revert";

const NIGHT_LIGHT_TEMPERATURES: &[(&str, u32)] = &[
    ("Subtle · 4500 K", 4_500),
    ("Warm · 3700 K", 3_700),
    ("Very warm · 2800 K", 2_800),
];

fn temperature_ftl(english: &str) -> &'static str {
    match english {
        "Subtle · 4500 K" => "display-night-light-temp-subtle",
        "Very warm · 2800 K" => "display-night-light-temp-very-warm",
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
        .map_or("Warm · 3700 K", |(label, _)| *label);
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
    pub color_profiles: Vec<String>,
    pub color_profile_labels: BTreeMap<String, String>,
    /// EDID identity; decides which saved arrangement applies.
    pub monitor: Option<MonitorIdentity>,
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

/// Map a raw DRM connector directory name + its `status` file contents to a
/// [`DisplayOutput`]. `conn` looks like `card0-eDP-1`; the `cardN-` prefix
/// is stripped and built-in panels (eDP/LVDS/DSI) get a friendly label.
pub fn classify_connector(conn: &str, status: &str) -> DisplayOutput {
    // Strip the leading `cardN-` so we're left with the connector, e.g.
    // `eDP-1`, `HDMI-A-1`, `DP-2`.
    let name = conn.split_once('-').map_or(conn, |(_, rest)| rest);
    let upper = name.to_ascii_uppercase();
    let builtin = upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
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
        color_profiles: Vec::new(),
        color_profile_labels: BTreeMap::new(),
        monitor: None,
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
/// cards with that label become e.g. `HDMI-A-1 · pci:0000:01:00.0` so every
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
        (false, Some(name)) => format!("{name} · {}", descriptor.label()),
        (false, None) => descriptor.label().to_string(),
    };
    descriptor
        .card_qualifier()
        .map_or_else(|| base.clone(), |card| format!("{base} · {card}"))
}

/// Enumerate real DRM connectors from `/sys/class/drm`. Best-effort: returns
/// an empty vec when the directory is unavailable (a container under
/// `--shots`), in which case the pane shows an honest "no displays" note.
/// Virtual connectors (compositor writeback) are skipped — they are not
/// displays a user can plug into.
fn read_outputs() -> Vec<DisplayOutput> {
    let mut out: Vec<_> = DisplayHardwareSnapshot::load()
        .into_iter()
        .flat_map(|snapshot| snapshot.outputs)
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
    output_brightness_from(outputs, &os_actions::BrightnessService::global().levels())
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

/// Display pane widget.
pub struct DisplayPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    outputs: Vec<DisplayOutput>,
    ambient_sensor_available: bool,
    initial_brightness: f32,
    initial_prefs: DisplayPrefs,
    last_brightness: f32,
    last_prefs: DisplayPrefs,
    output_config: DisplayConfig,
    /// External monitors whose brightness is set over DDC/CI, by output key,
    /// with their last known level. The built-in panel's backlight is the
    /// Brightness card's slider.
    output_brightness: BTreeMap<String, f32>,
    pending_candidate: Option<DisplayConfig>,
    last_seen_transaction: Option<(u64, DisplayTransactionState)>,
    last_snapshot_check: Instant,
    snapshot_stamp: Option<SystemTime>,
    hardware_probe: Option<std::sync::mpsc::Receiver<(f32, bool)>>,
    brightness_tx: Option<std::sync::mpsc::Sender<f32>>,
    brightness_result_rx: Option<std::sync::mpsc::Receiver<Result<f32, String>>>,
    pending_brightness: Option<(f32, f32)>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    require_operation_token: bool,
    needs_seed: bool,
    /// The arrangement canvas's in-progress drag; kept here because the
    /// composition is rebuilt between frames.
    arrangement_drag: ui::LocalState<Option<super::display_arrangement::ArrangementDrag>>,
    composed: Option<Box<dyn Widget>>,
    /// Hardware facts changed under the retained composition. The host holds
    /// raw pointers into it until the next build, so it must not be dropped.
    structure_dirty: bool,
    rect: Rect,
}

impl Default for DisplayPane {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayPane {
    pub fn new() -> Self {
        let initial = 0.5;
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("settings-display-probe".into())
            .spawn(move || {
                let _ = probe_tx.send((
                    os_actions::get_brightness().unwrap_or(0.5),
                    os_actions::has_ambient_light_sensor(),
                ));
            })
            .ok();
        let dispatcher = dispatcher_or_real(None);
        let (brightness_tx, brightness_rx) = std::sync::mpsc::channel::<f32>();
        let (brightness_result_tx, brightness_result_rx) = std::sync::mpsc::channel();
        let worker_dispatcher = Arc::clone(&dispatcher);
        std::thread::Builder::new()
            .name("settings-brightness-apply".into())
            .spawn(move || {
                while let Ok(percent) = brightness_rx.recv() {
                    let result = worker_dispatcher
                        .set_brightness(percent)
                        .map(|()| percent / 100.0);
                    let _ = brightness_result_tx.send(result);
                }
            })
            .ok();
        let outputs = read_outputs();
        let output_config = DisplayConfig::load_for(&Self::display_set_of(&outputs));
        let output_brightness = read_output_brightness(&outputs);
        Self {
            dispatcher,
            output_brightness,
            outputs,
            ambient_sensor_available: false,
            initial_brightness: initial,
            initial_prefs: SystemPrefs::load().display,
            last_brightness: initial,
            last_prefs: DisplayPrefs::default(),
            output_config,
            pending_candidate: None,
            last_seen_transaction: None,
            last_snapshot_check: ui::time::now(),
            snapshot_stamp: settings_store::hardware::display_snapshot_path()
                .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
            hardware_probe: Some(probe_rx),
            brightness_tx: Some(brightness_tx),
            brightness_result_rx: Some(brightness_result_rx),
            pending_brightness: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            needs_seed: true,
            arrangement_drag: ui::LocalState::new(None),
            composed: None,
            structure_dirty: false,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` constructor: inject the dispatcher, initial
    /// brightness, and a deterministic output list so the render/assertions
    /// don't depend on the host's hardware.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_brightness: f32,
    ) -> Self {
        Self {
            dispatcher,
            outputs: vec![DisplayOutput {
                label: "Built-in display".into(),
                connected: true,
                builtin: true,
                connector: "eDP-1".into(),
                connector_label: "eDP-1".into(),
                modes: vec![OutputMode {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                }],
                current_mode: Some(OutputMode {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                }),
                scale: 1.0,
                hdr: false,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                vrr: false,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                color_profiles: Vec::new(),
                color_profile_labels: BTreeMap::new(),
                monitor: None,
            }],
            ambient_sensor_available: true,
            initial_brightness,
            initial_prefs: DisplayPrefs::default(),
            last_brightness: initial_brightness,
            last_prefs: DisplayPrefs::default(),
            output_config: DisplayConfig::default(),
            output_brightness: BTreeMap::new(),
            pending_candidate: None,
            last_seen_transaction: None,
            last_snapshot_check: ui::time::now(),
            snapshot_stamp: None,
            hardware_probe: None,
            brightness_tx: None,
            brightness_result_rx: None,
            pending_brightness: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            needs_seed: true,
            arrangement_drag: ui::LocalState::new(None),
            composed: None,
            structure_dirty: false,
            rect: Rect::ZERO,
        }
    }

    /// Pin the output list (tests / `--shots`).
    #[must_use]
    pub fn with_outputs(mut self, outputs: Vec<DisplayOutput>) -> Self {
        self.outputs = outputs;
        self
    }

    /// Pin which monitors have a brightness control (tests / `--shots`).
    #[must_use]
    pub fn with_monitor_brightness(mut self, levels: &[os_actions::MonitorLevel]) -> Self {
        self.output_brightness = output_brightness_from(&self.outputs, levels);
        self
    }

    #[must_use]
    pub fn with_ambient_sensor(mut self, available: bool) -> Self {
        self.ambient_sensor_available = available;
        self
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self.require_operation_token = true;
        self
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    fn execute_local_command(
        &mut self,
        local: &mut AutoCommit,
        operation: crate::actions::DisplayOperation,
    ) {
        let root = automerge::ROOT;
        self.prepare_running_operation(local, &root, &operation);
        self.sync_brightness(local, &root);
        self.sync_output_brightness(local, &root);
        self.sync_comfort_prefs(local, &root, true);
        let connected: Vec<_> = self
            .outputs
            .iter()
            .filter(|o| o.connected)
            .cloned()
            .collect();
        self.sync_output_config(local, &root, &connected);
        self.sync_transaction_controls(local, &root);
    }

    fn finish_display_operation(&self, result: Result<String, String>) {
        let crate::actions::LocalOperationStatus::Running { token } = self.operation_status.get()
        else {
            return;
        };
        if !token.starts_with(crate::actions::DISPLAY_OPERATION_TYPE) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    fn running_operation(&self) -> Option<crate::actions::DisplayOperation> {
        if !self.require_operation_token {
            return None;
        }
        let crate::actions::LocalOperationStatus::Running { token } = self.operation_status.get()
        else {
            return None;
        };
        let payload =
            token.strip_prefix(&format!("{}:", crate::actions::DISPLAY_OPERATION_TYPE))?;
        serde_json::from_str(payload).ok()
    }

    /// Replace the pane's transient document controls with authoritative
    /// device state, then project only the operation admitted by the local
    /// executor. This makes concurrently received CRDT edits inert even while
    /// a legitimate display operation is in flight.
    fn prepare_running_operation(
        &self,
        state: &mut AutoCommit,
        root: &ObjId,
        operation: &crate::actions::DisplayOperation,
    ) {
        let _ = state.put(root, BRIGHTNESS_KEY, f64::from(self.last_brightness));
        let _ = state.put(root, AUTO_BRIGHTNESS_KEY, self.last_prefs.auto_brightness);
        let _ = state.put(root, DISPLAY_SCALE_KEY, scale_label(self.last_prefs.scale));
        let _ = state.put(root, APPLY_DISPLAY_KEY, false);
        let _ = state.put(root, KEEP_DISPLAY_KEY, false);
        let _ = state.put(root, REVERT_DISPLAY_KEY, false);
        let committed = self.output_config.clone();
        self.write_output_controls(state, root, &committed);

        use crate::actions::DisplayOperation as Op;
        match operation {
            Op::SetBrightness { value } => {
                let _ = state.put(root, BRIGHTNESS_KEY, *value);
            }
            Op::SetAutoBrightness { enabled } => {
                let _ = state.put(root, AUTO_BRIGHTNESS_KEY, *enabled);
            }
            Op::SetNightLight { enabled } => {
                let _ = state.put(root, NIGHT_LIGHT_KEY, *enabled);
            }
            Op::SetNightLightSchedule { enabled } => {
                let _ = state.put(root, NIGHT_LIGHT_SCHEDULE_KEY, *enabled);
            }
            Op::SetNightLightTemperature { label } => {
                let _ = state.put(root, NIGHT_LIGHT_TEMPERATURE_KEY, label.as_str());
            }
            Op::SetScale { label } => {
                let _ = state.put(root, DISPLAY_SCALE_KEY, label.as_str());
            }
            Op::SetEnabled { connector, enabled } => {
                let _ = state.put(root, output_key(connector, "enabled"), *enabled);
            }
            Op::SetOutputBrightness { connector, value } => {
                let _ = state.put(root, output_key(connector, "brightness"), *value);
            }
            Op::SetPrimary { connector, primary } => {
                let _ = state.put(root, output_key(connector, "primary"), *primary);
            }
            Op::SetOutputScale { connector, label } => {
                let _ = state.put(root, output_key(connector, "scale"), label.as_str());
            }
            Op::SetPosition { connector, label } => {
                let _ = state.put(root, output_key(connector, "position"), label.as_str());
                // A relative position replaces an exact one from a drag.
                let _ = state.delete(root, output_key(connector, "x"));
                let _ = state.delete(root, output_key(connector, "y"));
            }
            Op::MoveOutput { connector, x, y } => {
                let _ = state.put(root, output_key(connector, "x"), i64::from(*x));
                let _ = state.put(root, output_key(connector, "y"), i64::from(*y));
            }
            Op::ForgetDisplayTool { executable } => {
                self.finish_display_operation(
                    settings_store::display_tools::TrustedDisplayTools::forget(executable)
                        .map(|()| crate::i18n::t("display-tools-forgotten"))
                        .map_err(|error| format!("The display tool was not forgotten: {error}")),
                );
            }
            Op::Identify => {
                self.finish_display_operation(
                    settings_store::display_identify::IdentifyRequest::request()
                        .map(|_| "Each display shows its number.".into())
                        .map_err(|error| format!("Displays could not be identified: {error}")),
                );
            }
            Op::SetMirror { connector, label } => {
                let _ = state.put(root, output_key(connector, "mirror"), label.as_str());
            }
            Op::SetModeLabel { connector, label } => {
                let _ = state.put(root, output_key(connector, "mode"), label.as_str());
            }
            Op::SetTransformLabel { connector, label } => {
                let _ = state.put(root, output_key(connector, "transform"), label.as_str());
            }
            Op::SetColorProfileLabel { connector, label } => {
                let _ = state.put(root, output_key(connector, "color_profile"), label.as_str());
            }
            Op::SetMode { connector, mode } => {
                let _ = state.put(root, output_key(connector, "mode"), mode.label());
            }
            Op::SetTransform {
                connector,
                transform,
            } => {
                let _ = state.put(
                    root,
                    output_key(connector, "transform"),
                    transform_label(*transform),
                );
            }
            Op::SetHdr { connector, enabled } => {
                let _ = state.put(root, output_key(connector, "hdr"), *enabled);
            }
            Op::SetSdrBrightness { connector, value } => {
                let _ = state.put(root, output_key(connector, "sdr_brightness"), *value);
            }
            Op::SetVrr { connector, enabled } => {
                let _ = state.put(root, output_key(connector, "vrr"), *enabled);
            }
            Op::SetColorProfile { connector, profile } => {
                let label = profile.as_ref().map_or_else(
                    || "System default".to_string(),
                    |id| {
                        self.outputs
                            .iter()
                            .find(|output| output.connector == *connector)
                            .and_then(|output| output.color_profile_labels.get(id))
                            .map_or_else(|| id.clone(), |name| format!("{name} · {id}"))
                    },
                );
                let _ = state.put(root, output_key(connector, "color_profile"), label);
            }
            Op::Apply => {
                let _ = state.put(root, APPLY_DISPLAY_KEY, true);
            }
            Op::Confirm => {
                let _ = state.put(root, KEEP_DISPLAY_KEY, true);
            }
            Op::Revert => {
                let _ = state.put(root, REVERT_DISPLAY_KEY, true);
            }
        }
    }

    pub fn outputs(&self) -> &[DisplayOutput] {
        &self.outputs
    }

    /// The outputs the arrangement card lists as rows: only the connected
    /// ones. Disconnected physical ports are unused hardware, not displays —
    /// enumerating each one (a laptop's GPU can expose a dozen idle DP
    /// connectors) is noise, so they are summarised in a caption instead.
    fn connected_outputs(&self) -> Vec<&DisplayOutput> {
        self.outputs.iter().filter(|o| o.connected).collect()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let connected = self.connected_outputs();
        let arrangement = self.build_arrangement_card(&connected);
        let brightness = self.build_brightness_card();

        let mut cards = vec![arrangement];
        for (index, output) in connected.iter().enumerate() {
            cards.push(self.build_output_card(index, output, &connected));
        }
        cards.push(self.build_safety_card());
        if let Some(tools) = Self::build_trusted_tools_card() {
            cards.push(tools);
        }
        cards.push(brightness);
        crate::widgets::cards(cards)
    }

    /// The "Displays" arrangement card: a drag-to-arrange canvas of the
    /// connected outputs with an Identify button, plus a caption about idle
    /// ports. See [`Self::connected_outputs`] for why disconnected ports
    /// aren't enumerated.
    fn build_arrangement_card(&self, connected: &[&DisplayOutput]) -> El {
        if connected.is_empty() {
            return card(
                &t("display-card-title"),
                rows(vec![caption(&t("display-none"))]),
            );
        }
        let disconnected = self.outputs.len() - connected.len();
        let displays = self.arranged_displays(connected);
        let moves = displays
            .iter()
            .filter(|display| display.movable)
            .filter_map(|display| {
                crate::actions::display_output_move_binding(display.connector.clone())
                    .map(|binding| (display.connector.clone(), binding))
            })
            .collect();
        let mut out_rows: Vec<El> = vec![
            Box::new(super::display_arrangement::DisplayArrangement::new(
                displays,
                moves,
                self.arrangement_drag.clone(),
            )),
            caption(&crate::i18n::t("display-arrangement-hint")),
            setting_row(
                &crate::i18n::t("display-identify-row"),
                Box::new(
                    Button::new(
                        "settings.display.identify",
                        &crate::i18n::t("display-identify"),
                        None,
                    )
                    .content_sized()
                    .on_action(
                        crate::actions::display_unit_binding("identify")
                            .expect("display identify action is published"),
                    ),
                ),
            ),
        ];
        // One honest line about the idle ports rather than a row each.
        out_rows.push(caption(&if connected.len() > 1 {
            t("display-caption-switch")
        } else if disconnected == 0 {
            t("display-caption-second")
        } else {
            t("display-caption-port")
        }));
        card(&t("display-card-title"), rows(out_rows))
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
    fn build_brightness_card(&self) -> El {
        let automatic_brightness = if self.ambient_sensor_available {
            setting_row(
                &t("display-auto-brightness"),
                Box::new(
                    Toggle::new("settings.display.auto_brightness", AUTO_BRIGHTNESS_KEY)
                        .with_id("display.auto_brightness")
                        .on_action(
                            crate::actions::display_bool_binding("auto_brightness").expect(
                                "automatic brightness is part of the static resource contract",
                            ),
                        ),
                ),
            )
        } else {
            value_row(&t("display-auto-brightness"), &t("display-no-ambient"))
        };
        card(
            &t("display-brightness"),
            rows(vec![
                setting_row(
                    &t("display-brightness"),
                    Box::new(
                        Slider::new("settings.display.brightness", BRIGHTNESS_KEY)
                            .with_id("display.brightness")
                            .on_action(
                                crate::actions::display_number_binding("brightness")
                                    .expect("brightness is part of the static resource contract"),
                            ),
                    ),
                ),
                automatic_brightness,
                setting_row(
                    &t("display-night-light"),
                    Box::new(
                        Toggle::new("settings.display.night_light", NIGHT_LIGHT_KEY)
                            .with_id("display.night_light")
                            .on_action(
                                crate::actions::display_bool_binding("night_light")
                                    .expect("Night Light is part of the static resource contract"),
                            ),
                    ),
                ),
                setting_row_described(
                    &t("display-night-light-schedule"),
                    &t("display-night-light-schedule-desc"),
                    Box::new(
                        Toggle::new(
                            "settings.display.night_light_schedule",
                            NIGHT_LIGHT_SCHEDULE_KEY,
                        )
                        .with_id("display.night_light_schedule")
                        .on_action(
                            crate::actions::display_bool_binding("night_light_schedule").expect(
                                "the Night Light schedule is part of the static resource contract",
                            ),
                        ),
                    ),
                ),
                choice_row(
                    &t("display-night-light-warmth"),
                    Box::new(
                        Dropdown::new(
                            temperature_options(),
                            NIGHT_LIGHT_TEMPERATURE_KEY,
                        )
                        .with_id("display.night_light_temperature")
                        .on_action(
                            crate::actions::display_choice_binding("night_light_temperature")
                                .expect(
                                    "Night Light warmth is part of the static resource contract",
                                ),
                        )
                        .with_placeholder(&temperature_label(
                            self.initial_prefs.night_light_temperature,
                        )),
                    ),
                ),
                caption(&t("display-night-light-caption")),
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
    ) -> El {
        let config = self
            .output_config
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
            output_rows.push(setting_row(
                &t("display-brightness"),
                Box::new(
                    Slider::new(
                        &format!("settings.display.{}.brightness", output.connector),
                        &output_key(&output.connector, "brightness"),
                    )
                    .with_id(&output_key(&output.connector, "brightness"))
                    .on_action(
                        crate::actions::display_output_brightness_binding(output.connector.clone())
                            .expect("monitor brightness is part of the resource contract"),
                    ),
                ),
            ));
        }
        output_rows.extend(Self::build_output_primary_rows(output, connected, &config));
        output_rows.extend(Self::build_output_mode_and_rotation_rows(output, &config));
        output_rows.extend(Self::build_output_capability_rows(output, &config));
        card(&output_title(output), rows(output_rows))
    }

    /// Enable/primary toggles + scale/position/mirroring dropdowns.
    fn build_output_primary_rows(
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
        config: &OutputConfig,
    ) -> Vec<El> {
        let primary_key = output_key(&output.connector, "primary");
        let enabled_key = output_key(&output.connector, "enabled");
        let scale_key = output_key(&output.connector, "scale");
        let position_key = output_key(&output.connector, "position");
        let mirror_key = output_key(&output.connector, "mirror");
        let toggle_id = format!("settings.display.{}.primary", output.connector);
        let scale_choices: Vec<String> = DISPLAY_SCALES
            .iter()
            .map(|(label, _)| (*label).into())
            .collect();
        let position_choices: Vec<String> = std::iter::once("Automatic".into())
            .chain(
                connected
                    .iter()
                    .filter(|candidate| candidate.connector != output.connector)
                    .flat_map(|candidate| {
                        ["Left", "Right", "Above", "Below"]
                            .map(|side| format!("{side} of {}", candidate.label))
                    }),
            )
            .collect();
        let mirror_choices: Vec<String> = std::iter::once("Extend workspace".into())
            .chain(
                connected
                    .iter()
                    .filter(|candidate| candidate.connector != output.connector)
                    .map(|candidate| format!("Mirror {}", candidate.label)),
            )
            .collect();
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
                &t("display-use"),
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.enabled", output.connector),
                        &enabled_key,
                    )
                    .with_id(&enabled_key)
                    .on_action(
                        crate::actions::display_output_bool_binding(
                            "enabled",
                            output.connector.clone(),
                        )
                        .expect("output enabled action is part of the resource contract"),
                    ),
                ),
            ),
            setting_row(
                &t("display-primary"),
                Box::new(
                    Toggle::new(&toggle_id, &primary_key)
                        .with_id(&primary_key)
                        .on_action(
                            crate::actions::display_output_bool_binding(
                                "primary",
                                output.connector.clone(),
                            )
                            .expect("primary output action is part of the resource contract"),
                        ),
                ),
            ),
            choice_row(
                &t("display-scale"),
                Box::new(
                    Dropdown::new(scale_choices.clone(), &scale_key)
                        .with_id(&scale_key)
                        .on_action(
                            crate::actions::display_output_choice_binding(
                                "scale",
                                output.connector.clone(),
                                scale_choices,
                            )
                            .expect("output scale action is part of the resource contract"),
                        )
                        .with_placeholder(scale_label(config.scale)),
                ),
            ),
            choice_row(
                &t("display-position"),
                Box::new(
                    Dropdown::new(position_choices.clone(), &position_key)
                        .with_id(&position_key)
                        .on_action(
                            crate::actions::display_output_choice_binding(
                                "position",
                                output.connector.clone(),
                                position_choices,
                            )
                            .expect("output position action is part of the resource contract"),
                        )
                        .with_placeholder("Automatic"),
                ),
            ),
            choice_row(
                &t("display-mirroring"),
                Box::new(
                    Dropdown::new(mirror_choices.clone(), &mirror_key)
                        .with_id(&mirror_key)
                        .on_action(
                            crate::actions::display_output_choice_binding(
                                "mirror",
                                output.connector.clone(),
                                mirror_choices,
                            )
                            .expect("output mirror action is part of the resource contract"),
                        )
                        .with_placeholder(&mirror_placeholder),
                ),
            ),
        ]
    }

    /// Resolution/refresh-rate dropdown (or an unavailable note) + rotation.
    fn build_output_mode_and_rotation_rows(
        output: &DisplayOutput,
        config: &OutputConfig,
    ) -> Vec<El> {
        let mode_key = output_key(&output.connector, "mode");
        let transform_key = output_key(&output.connector, "transform");
        let mut out_rows = Vec::new();
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
            out_rows.push(choice_row(
                "Resolution & refresh rate",
                Box::new(
                    Dropdown::new(mode_choices.clone(), &mode_key)
                        .with_id(&mode_key)
                        .on_action(
                            crate::actions::display_output_choice_binding(
                                "mode",
                                output.connector.clone(),
                                mode_choices,
                            )
                            .expect("output mode action is part of the resource contract"),
                        )
                        .with_placeholder(&mode_placeholder),
                ),
            ));
        }
        let rotation = transform_options();
        let rotation_current = transform_label(config.transform);
        out_rows.push(choice_row(
            &t("display-rotation"),
            Box::new(
                Dropdown::new(rotation.clone(), &transform_key)
                    .with_id(&transform_key)
                    .on_action(
                        crate::actions::display_output_choice_binding(
                            "transform",
                            output.connector.clone(),
                            rotation,
                        )
                        .expect("output transform action is part of the resource contract"),
                    )
                    .with_placeholder(&rotation_current),
            ),
        ));
        out_rows
    }

    /// HDR / variable-refresh-rate / colour-profile rows.
    fn build_output_capability_rows(output: &DisplayOutput, config: &OutputConfig) -> Vec<El> {
        let hdr_key = output_key(&output.connector, "hdr");
        let vrr_key = output_key(&output.connector, "vrr");
        let profile_key = output_key(&output.connector, "color_profile");
        let mut out_rows = Vec::new();

        if output.hdr {
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr"),
                &crate::i18n::t("display-hdr-description"),
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.hdr", output.connector),
                        &hdr_key,
                    )
                    .with_id(&hdr_key)
                    .on_action(
                        crate::actions::display_output_bool_binding(
                            "hdr",
                            output.connector.clone(),
                        )
                        .expect("HDR action is part of the resource contract"),
                    ),
                ),
            ));
            let sdr_key = output_key(&output.connector, "sdr_brightness");
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr-sdr-brightness"),
                &crate::i18n::t("display-hdr-sdr-brightness-description"),
                Box::new(
                    Slider::new(
                        &format!("settings.display.{}.sdr_brightness", output.connector),
                        &sdr_key,
                    )
                    .with_id(&sdr_key)
                    .on_action(
                        crate::actions::display_output_sdr_brightness_binding(
                            output.connector.clone(),
                        )
                        .expect("SDR brightness is part of the resource contract"),
                    ),
                ),
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
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.vrr", output.connector),
                        &vrr_key,
                    )
                    .with_id(&vrr_key)
                    .on_action(
                        crate::actions::display_output_bool_binding(
                            "vrr",
                            output.connector.clone(),
                        )
                        .expect("VRR action is part of the resource contract"),
                    ),
                ),
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
                    .map(|label| format!("{label} · {id}"))
            }));
            let placeholder = config
                .color_profile
                .as_ref()
                .and_then(|id| {
                    output
                        .color_profile_labels
                        .get(id)
                        .map(|label| format!("{label} · {id}"))
                })
                .unwrap_or_else(|| "System default".into());
            out_rows.push(choice_row(
                &t("display-colour-profile"),
                Box::new(
                    Dropdown::new(choices.clone(), &profile_key)
                        .with_id(&profile_key)
                        .on_action(
                            crate::actions::display_output_choice_binding(
                                "color-profile",
                                output.connector.clone(),
                                choices,
                            )
                            .expect("colour profile action is part of the resource contract"),
                        )
                        .with_placeholder(&placeholder),
                ),
            ));
        }
        out_rows
    }

    /// The safety/apply-transaction card, driven by the on-disk
    /// [`DisplayTransaction`] state machine.
    fn build_safety_card(&self) -> El {
        let transaction = DisplayTransaction::load();
        match transaction.as_ref().map(|transaction| transaction.state) {
            Some(DisplayTransactionState::AwaitingConfirmation) => {
                let seconds = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.deadline_unix_ms)
                    .map(|deadline| deadline.saturating_sub(unix_now_ms()).div_ceil(1_000))
                    .unwrap_or_default();
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                let keep = t("display-keep");
                let revert = t("display-revert-now");
                card(
                    &t("display-keep-title"),
                    rows(vec![
                        caption(&t_args(
                            "display-keep-caption",
                            &[("seconds", seconds.into())],
                        )),
                        setting_row(
                            &t("display-keep-ok"),
                            Box::new(
                                Button::new("settings.display.keep", &keep, None)
                                    .content_sized()
                                    .on_action(
                                        crate::actions::display_unit_binding("confirm")
                                            .expect("display confirm action is published"),
                                    ),
                            ),
                        ),
                        setting_row(
                            &t("display-keep-wrong"),
                            Box::new(
                                Button::new("settings.display.revert", &revert, None)
                                    .content_sized()
                                    .on_action(
                                        crate::actions::display_unit_binding("revert")
                                            .expect("display revert action is published"),
                                    ),
                            ),
                        ),
                    ]),
                )
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
                card(&t("display-applying-title"), rows(vec![caption(&applying)]))
            }
            Some(DisplayTransactionState::Rejected) => {
                let rejected = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.message.as_deref())
                    .map_or_else(|| t("display-rejected-caption"), str::to_string);
                let mut content = vec![caption(&rejected)];
                if self.pending_candidate.is_some() {
                    let try_again = t("display-try-again");
                    content.push(setting_row(
                        &t("display-retry"),
                        Box::new(
                            Button::new("settings.display.apply", &try_again, None)
                                .content_sized()
                                .on_action(
                                    crate::actions::display_unit_binding("apply")
                                        .expect("display apply action is published"),
                                ),
                        ),
                    ));
                }
                card(&t("display-rejected-title"), rows(content))
            }
            _ if self.pending_candidate.is_some() => {
                let apply = t("display-apply");
                card(
                    &t("display-apply-title"),
                    rows(vec![
                        caption(&t("display-apply-caption")),
                        setting_row(
                            &t("display-apply-row"),
                            Box::new(
                                Button::new("settings.display.apply", &apply, None)
                                    .content_sized()
                                    .on_action(
                                        crate::actions::display_unit_binding("apply")
                                            .expect("display apply action is published"),
                                    ),
                            ),
                        ),
                    ]),
                )
            }
            _ => card(
                &t("display-config-title"),
                rows(vec![caption(&t("display-config-empty"))]),
            ),
        }
    }

    /// Layout tools whose display changes are kept without asking, each
    /// with a way to be asked again. Absent while no tool is trusted.
    fn build_trusted_tools_card() -> Option<El> {
        let trusted = settings_store::display_tools::TrustedDisplayTools::load();
        if trusted.tools.is_empty() {
            return None;
        }
        let mut content = vec![caption(&crate::i18n::t("display-tools-caption"))];
        for tool in &trusted.tools {
            let Some(binding) = crate::actions::display_tool_forget_binding(tool) else {
                continue;
            };
            content.push(setting_row(
                tool.name(),
                Box::new(
                    Button::new(
                        "settings.display.tool.forget",
                        &crate::i18n::t("display-tools-forget"),
                        None,
                    )
                    .on_action(binding),
                ),
            ));
        }
        Some(card(&crate::i18n::t("display-tools-title"), rows(content)))
    }

    /// Load prefs fresh, apply `f` to the display section, persist.
    fn persist_display(f: impl FnOnce(&mut DisplayPrefs)) -> Result<(), String> {
        let mut p = SystemPrefs::load();
        f(&mut p.display);
        p.save().map(|_| ()).map_err(|e| {
            warn!(target: "tilo::settings", op = "display_prefs_save", error = %e, "failed to persist display prefs");
            format!("Display preference could not be saved: {e}")
        })
    }

    fn write_output_controls(&self, state: &mut AutoCommit, root: &ObjId, config: &DisplayConfig) {
        let connected = self.connected_outputs();
        for (index, output) in connected.iter().enumerate() {
            let cfg = config
                .outputs
                .get(&output.connector)
                .cloned()
                .unwrap_or_else(|| OutputConfig {
                    primary: index == 0,
                    x: index as i32 * 1920,
                    ..output.unconfigured()
                });
            let _ = state.put(root, output_key(&output.connector, "enabled"), cfg.enabled);
            let _ = state.put(root, output_key(&output.connector, "primary"), cfg.primary);
            if let Some(level) = self.output_brightness.get(&output.connector) {
                let _ = state.put(
                    root,
                    output_key(&output.connector, "brightness"),
                    f64::from(*level),
                );
            }
            let _ = state.put(
                root,
                output_key(&output.connector, "scale"),
                scale_label(cfg.scale),
            );
            let _ = state.put(
                root,
                output_key(&output.connector, "transform"),
                transform_label(cfg.transform),
            );
            let _ = state.put(root, output_key(&output.connector, "hdr"), cfg.hdr);
            let _ = state.put(
                root,
                output_key(&output.connector, "sdr_brightness"),
                sdr_brightness_fraction(cfg.sdr_brightness_nits),
            );
            let _ = state.put(root, output_key(&output.connector, "vrr"), cfg.vrr);
            let profile = cfg
                .color_profile
                .as_ref()
                .and_then(|id| {
                    output
                        .color_profile_labels
                        .get(id)
                        .map(|label| format!("{label} · {id}"))
                })
                .unwrap_or_else(|| "System default".into());
            let _ = state.put(
                root,
                output_key(&output.connector, "color_profile"),
                profile,
            );
            if let Some(mode) = cfg.mode.or(output.current_mode) {
                let _ = state.put(root, output_key(&output.connector, "mode"), mode.label());
            }
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(DisplayPane::new())
}

impl Widget for DisplayPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        self.poll_brightness_result(&mut state, &root);
        self.poll_hardware_probe(&mut state, &root);
        self.refresh_outputs_if_stale();
        if self.needs_seed {
            self.seed_state(&mut state, &root);
        }
        if let Some(crate::actions::LocalPaneCommand::Display(operation)) = self.command_state.get()
        {
            self.command_state.set(None);
            self.execute_local_command(&mut state, operation);
        }
        let running_operation = self.running_operation();
        if let Some(operation) = &running_operation {
            self.prepare_running_operation(&mut state, &root, operation);
        } else if self.require_operation_token {
            let committed = self.output_config.clone();
            self.write_output_controls(&mut state, &root, &committed);
            let _ = state.put(&root, BRIGHTNESS_KEY, f64::from(self.last_brightness));
            let _ = state.put(&root, AUTO_BRIGHTNESS_KEY, self.last_prefs.auto_brightness);
            let _ = state.put(&root, DISPLAY_SCALE_KEY, scale_label(self.last_prefs.scale));
            let _ = state.put(&root, APPLY_DISPLAY_KEY, false);
            let _ = state.put(&root, KEEP_DISPLAY_KEY, false);
            let _ = state.put(&root, REVERT_DISPLAY_KEY, false);
        }
        let has_pending_operation = !self.require_operation_token || running_operation.is_some();
        if has_pending_operation {
            self.sync_brightness(&mut state, &root);
            self.sync_output_brightness(&mut state, &root);
        }
        self.sync_comfort_prefs(&mut state, &root, has_pending_operation);

        let connected: Vec<_> = self
            .outputs
            .iter()
            .filter(|o| o.connected)
            .cloned()
            .collect();
        if has_pending_operation {
            self.sync_output_config(&mut state, &root, &connected);
            self.sync_transaction_controls(&mut state, &root);
        }

        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "child was just created in this same tree"
        )]
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "DisplayPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|c| c.take_structure_dirty())
    }
}

/// `sync` helper methods, split out of the trait method above (which must
/// stay a plain dispatcher) purely to stay under the line-count lint.
impl DisplayPane {
    /// Drain the background brightness/ambient-sensor probe, seeding the
    /// CRDT and dropping the receiver once it has reported (or hung up).
    fn poll_hardware_probe(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(receiver) = &self.hardware_probe else {
            return;
        };
        match receiver.try_recv() {
            Ok((brightness, ambient)) => {
                self.initial_brightness = brightness;
                self.last_brightness = brightness;
                self.ambient_sensor_available = ambient;
                let _ = state.put(
                    root,
                    BRIGHTNESS_KEY,
                    automerge::ScalarValue::F64(f64::from(brightness)),
                );
                self.hardware_probe = None;
                self.structure_dirty = true;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.hardware_probe = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Re-read the DRM connector snapshot when its mtime has moved, at most
    /// once a second.
    fn refresh_outputs_if_stale(&mut self) {
        if self.last_snapshot_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_snapshot_check = ui::time::now();
        let stamp = settings_store::hardware::display_snapshot_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        if stamp != self.snapshot_stamp {
            self.snapshot_stamp = stamp;
            self.outputs = read_outputs();
            self.output_config = DisplayConfig::load_for(&self.display_set());
            self.needs_seed = true;
            self.structure_dirty = true;
        }
        // Monitor probing finishes in the background; a monitor that turns
        // out to speak DDC/CI gains its slider once the probe lands.
        let controllable = read_output_brightness(&self.outputs);
        if controllable.keys().ne(self.output_brightness.keys()) {
            self.output_brightness = controllable;
            self.needs_seed = true;
            self.structure_dirty = true;
        }
    }

    /// First-sync seed: writes the initial brightness/comfort preferences
    /// and every connected output's current config into the CRDT.
    fn seed_state(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let _ = state.put(
            root,
            BRIGHTNESS_KEY,
            automerge::ScalarValue::F64(f64::from(self.initial_brightness)),
        );
        let _ = state.put(
            root,
            AUTO_BRIGHTNESS_KEY,
            automerge::ScalarValue::Boolean(self.initial_prefs.auto_brightness),
        );
        let _ = state.put(
            root,
            NIGHT_LIGHT_KEY,
            automerge::ScalarValue::Boolean(self.initial_prefs.night_light),
        );
        let _ = state.put(
            root,
            NIGHT_LIGHT_SCHEDULE_KEY,
            automerge::ScalarValue::Boolean(self.initial_prefs.night_light_schedule),
        );
        let _ = state.put(
            root,
            NIGHT_LIGHT_TEMPERATURE_KEY,
            temperature_label(self.initial_prefs.night_light_temperature),
        );
        let _ = state.put(
            root,
            DISPLAY_SCALE_KEY,
            scale_label(self.initial_prefs.scale),
        );
        let _ = state.put(root, APPLY_DISPLAY_KEY, false);
        let _ = state.put(root, KEEP_DISPLAY_KEY, false);
        let _ = state.put(root, REVERT_DISPLAY_KEY, false);
        let connected: Vec<_> = self
            .outputs
            .iter()
            .filter(|o| o.connected)
            .cloned()
            .collect();
        for (index, output) in connected.iter().enumerate() {
            let cfg = self
                .output_config
                .outputs
                .entry(output.connector.clone())
                .or_insert_with(|| OutputConfig {
                    primary: index == 0,
                    x: index as i32 * 1920,
                    ..Default::default()
                });
            let _ = state.put(root, output_key(&output.connector, "primary"), cfg.primary);
            let _ = state.put(root, output_key(&output.connector, "enabled"), cfg.enabled);
            let _ = state.put(
                root,
                output_key(&output.connector, "scale"),
                scale_label(cfg.scale),
            );
            let _ = state.put(root, output_key(&output.connector, "position"), "Automatic");
            let mirror = cfg
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
            let _ = state.put(root, output_key(&output.connector, "mirror"), mirror);
            if let Some(mode) = cfg.mode {
                let _ = state.put(root, output_key(&output.connector, "mode"), mode.label());
            }
            let _ = state.put(
                root,
                output_key(&output.connector, "transform"),
                transform_label(cfg.transform),
            );
            let _ = state.put(root, output_key(&output.connector, "hdr"), cfg.hdr);
            let _ = state.put(root, output_key(&output.connector, "vrr"), cfg.vrr);
            let profile = cfg
                .color_profile
                .as_ref()
                .and_then(|id| {
                    output
                        .color_profile_labels
                        .get(id)
                        .map(|label| format!("{label} · {id}"))
                })
                .unwrap_or_else(|| "System default".into());
            let _ = state.put(
                root,
                output_key(&output.connector, "color_profile"),
                &profile,
            );
        }
        self.last_prefs = self.initial_prefs;
        self.needs_seed = false;
    }

    /// Brightness → dispatcher, guarded so we don't shell out every frame.
    fn sync_brightness(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, BRIGHTNESS_KEY)
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::F64(f) = s.as_ref()
        {
            let new = *f as f32;
            if (new - self.last_brightness).abs() > 0.001 && self.pending_brightness.is_none() {
                let percent = new.clamp(0.0, 1.0) * 100.0;
                if let Some(sender) = &self.brightness_tx {
                    if sender.send(percent).is_ok() {
                        self.pending_brightness = Some((self.last_brightness, new));
                    } else {
                        let _ = state.put(root, BRIGHTNESS_KEY, f64::from(self.last_brightness));
                        self.finish_display_operation(Err(
                            "Brightness worker is unavailable; the display was not changed.".into(),
                        ));
                    }
                } else {
                    match self.dispatcher.set_brightness(percent) {
                        Ok(()) => {
                            self.last_brightness = new;
                            self.finish_display_operation(Ok(format!(
                                "Brightness set to {}%.",
                                percent.round()
                            )));
                        }
                        Err(e) => {
                            let _ =
                                state.put(root, BRIGHTNESS_KEY, f64::from(self.last_brightness));
                            self.finish_display_operation(Err(format!(
                                "Brightness could not be changed: {e}"
                            )));
                            warn!(target: "tilo::settings", op = "set_brightness", error = %e, "display pane dispatch failed");
                        }
                    }
                }
            }
        }
    }

    /// Each external monitor's slider → its brightness over DDC/CI. The
    /// write is queued, not awaited: a monitor needs tens of milliseconds
    /// per command and the slider must keep up with the pointer.
    fn sync_output_brightness(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let changed: Vec<(String, f32)> = self
            .output_brightness
            .iter()
            .filter_map(|(connector, last)| {
                let Ok(Some((Value::Scalar(value), _))) =
                    state.get(root, output_key(connector, "brightness"))
                else {
                    return None;
                };
                let automerge::ScalarValue::F64(level) = value.as_ref() else {
                    return None;
                };
                let level = (*level as f32).clamp(0.0, 1.0);
                ((level - last).abs() > 0.001).then(|| (connector.clone(), level))
            })
            .collect();
        for (connector, level) in changed {
            let Some(label) = self
                .outputs
                .iter()
                .find(|output| output.connector == connector)
                .map(|output| output.connector_label.clone())
            else {
                continue;
            };
            match self
                .dispatcher
                .set_display_brightness(&label, level * 100.0)
            {
                Ok(()) => {
                    self.output_brightness.insert(connector, level);
                    self.finish_display_operation(Ok(format!(
                        "Brightness set to {}%.",
                        (level * 100.0).round()
                    )));
                }
                Err(error) => {
                    let previous = self
                        .output_brightness
                        .get(&connector)
                        .copied()
                        .unwrap_or_default();
                    let _ = state.put(
                        root,
                        output_key(&connector, "brightness"),
                        f64::from(previous),
                    );
                    self.finish_display_operation(Err(format!(
                        "Brightness could not be changed: {error}"
                    )));
                    warn!(target: "tilo::settings", op = "set_display_brightness", %error, "display pane dispatch failed");
                }
            }
        }
    }

    fn poll_brightness_result(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let Some(receiver) = &self.brightness_result_rx else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        let Some((previous, requested)) = self.pending_brightness.take() else {
            return;
        };
        match result {
            Ok(applied) => {
                self.last_brightness = applied;
                self.finish_display_operation(Ok(format!(
                    "Brightness set to {}%.",
                    (applied * 100.0).round()
                )));
            }
            Err(error) => {
                self.last_brightness = previous;
                let _ = state.put(root, BRIGHTNESS_KEY, f64::from(previous));
                self.finish_display_operation(Err(format!(
                    "Brightness could not be changed: {error}"
                )));
                warn!(target: "tilo::settings", op = "set_brightness", %error, requested, "display worker dispatch failed");
            }
        }
    }

    /// Comfort preferences → persist to the shared store on change.
    fn sync_comfort_prefs(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        has_pending_operation: bool,
    ) {
        if has_pending_operation
            && let Some(b) = read_bool(state, root, AUTO_BRIGHTNESS_KEY)
            && b ^ self.last_prefs.auto_brightness
        {
            let effective = b && self.ambient_sensor_available;
            if b && !self.ambient_sensor_available {
                let _ = state.put(root, AUTO_BRIGHTNESS_KEY, false);
                self.finish_display_operation(Err(
                    "Automatic brightness is unavailable because this device has no ambient-light sensor."
                        .into(),
                ));
            } else {
                match Self::persist_display(|d| d.auto_brightness = effective) {
                    Ok(()) => {
                        self.last_prefs.auto_brightness = effective;
                        self.finish_display_operation(Ok(if effective {
                            "Automatic brightness enabled.".into()
                        } else {
                            "Automatic brightness disabled.".into()
                        }));
                    }
                    Err(error) => {
                        let _ =
                            state.put(root, AUTO_BRIGHTNESS_KEY, self.last_prefs.auto_brightness);
                        self.finish_display_operation(Err(error));
                    }
                }
            }
        }
        if let Some(b) = read_bool(state, root, NIGHT_LIGHT_KEY)
            && b ^ self.last_prefs.night_light
        {
            self.last_prefs.night_light = b;
            let _ = Self::persist_display(|d| d.night_light = b);
        }
        if let Some(b) = read_bool(state, root, NIGHT_LIGHT_SCHEDULE_KEY)
            && b ^ self.last_prefs.night_light_schedule
        {
            self.last_prefs.night_light_schedule = b;
            let _ = Self::persist_display(|d| d.night_light_schedule = b);
        }
        if let Some(label) = read_str_or_text(state, root, NIGHT_LIGHT_TEMPERATURE_KEY)
            && let Some(kelvin) = temperature_from_label(&label)
            && kelvin != self.last_prefs.night_light_temperature
        {
            self.last_prefs.night_light_temperature = kelvin;
            let _ = Self::persist_display(|d| d.night_light_temperature = kelvin);
        }
        // Display scale → persist to the shared store on change. The
        // compositor reads `display.scale` at session start and advertises
        // it as the preferred fractional scale.
        if has_pending_operation
            && let Some(label) = read_str_or_text(state, root, DISPLAY_SCALE_KEY)
            && let Some(v) = scale_factor_of(&label)
            && (v - self.last_prefs.scale).abs() > f64::EPSILON
        {
            match Self::persist_display(|d| d.scale = v) {
                Ok(()) => {
                    self.last_prefs.scale = v;
                    self.finish_display_operation(Ok(format!("Display scale set to {label}.")));
                }
                Err(error) => {
                    let _ = state.put(root, DISPLAY_SCALE_KEY, scale_label(self.last_prefs.scale));
                    self.finish_display_operation(Err(error));
                }
            }
        }
    }

    /// Per-output intent is staged in a clone and only atomically saved
    /// after capability validation. Invalid/transient CRDT values never
    /// reach the compositor.
    fn sync_output_config(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        connected: &[DisplayOutput],
    ) {
        let mut next = self.output_config.clone();
        let mut changed = false;
        for output in connected {
            let mut cfg = next
                .outputs
                .get(&output.connector)
                .cloned()
                .unwrap_or_else(|| output.unconfigured());
            if Self::apply_output_field_overrides(state, root, output, &mut next, &mut cfg) {
                changed = true;
            }
            if Self::apply_output_placement_overrides(
                state, root, output, connected, &next, &mut cfg,
            ) {
                changed = true;
            }
            next.outputs.insert(output.connector.clone(), cfg);
        }
        self.apply_primary_selection(state, root, connected, &mut next, &mut changed);
        Self::apply_mirror_pairs(connected, &mut next, &mut changed);
        if changed {
            self.stage_output_candidate(connected, next);
            self.finish_display_operation(Ok(
                "Display change staged. Apply it to preview the new configuration.".into(),
            ));
        }
    }

    /// Enable/primary/scale/mode/transform/HDR/VRR/colour-profile overrides
    /// for one output. Returns whether anything changed.
    fn apply_output_field_overrides(
        state: &AutoCommit,
        root: &ObjId,
        output: &DisplayOutput,
        next: &mut DisplayConfig,
        cfg: &mut OutputConfig,
    ) -> bool {
        let mut changed = false;
        if let Some(enabled) = read_bool(state, root, output_key(&output.connector, "enabled"))
            && enabled != cfg.enabled
        {
            cfg.enabled = enabled;
            if !enabled {
                cfg.primary = false;
            }
            changed = true;
        }
        if let Some(primary) = read_bool(state, root, output_key(&output.connector, "primary"))
            && primary != cfg.primary
        {
            if primary {
                for other in next.outputs.values_mut() {
                    other.primary = false;
                }
            }
            cfg.primary = primary;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, output_key(&output.connector, "scale"))
            && let Some(scale) = scale_factor_of(&label)
            && (scale - cfg.scale).abs() > f64::EPSILON
        {
            cfg.scale = scale;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, output_key(&output.connector, "mode"))
            && let Some(mode) = output
                .modes
                .iter()
                .copied()
                .find(|mode| mode.label() == label)
            && cfg.mode != Some(mode)
        {
            cfg.mode = Some(mode);
            changed = true;
        }
        if let Some(label) =
            read_str_or_text(state, root, output_key(&output.connector, "transform"))
            && let Some(transform) = transform_from_label(&label)
            && transform != cfg.transform
        {
            cfg.transform = transform;
            changed = true;
        }
        if output.hdr
            && let Some(enabled) = read_bool(state, root, output_key(&output.connector, "hdr"))
            && enabled != cfg.hdr
        {
            cfg.hdr = enabled;
            changed = true;
        }
        if output.hdr
            && let Some(fraction) =
                ui::doc::read_f64(state, root, output_key(&output.connector, "sdr_brightness"))
            && sdr_brightness_nits(fraction) != cfg.sdr_brightness_nits
        {
            cfg.sdr_brightness_nits = sdr_brightness_nits(fraction);
            changed = true;
        }
        if output.vrr
            && let Some(enabled) = read_bool(state, root, output_key(&output.connector, "vrr"))
            && enabled != cfg.vrr
        {
            cfg.vrr = enabled;
            changed = true;
        }
        if let Some(label) =
            read_str_or_text(state, root, output_key(&output.connector, "color_profile"))
        {
            let selected = if label == "System default" {
                None
            } else {
                output
                    .color_profile_labels
                    .iter()
                    .find_map(|(id, candidate)| {
                        (label == format!("{candidate} · {id}")).then(|| id.clone())
                    })
            };
            if selected != cfg.color_profile {
                cfg.color_profile = selected;
                changed = true;
            }
        }
        changed
    }

    /// Mirror-target/relative-position overrides for one output. Returns
    /// whether anything changed.
    fn apply_output_placement_overrides(
        state: &AutoCommit,
        root: &ObjId,
        output: &DisplayOutput,
        connected: &[DisplayOutput],
        next: &DisplayConfig,
        cfg: &mut OutputConfig,
    ) -> bool {
        let mut changed = false;
        if let Some(label) = read_str_or_text(state, root, output_key(&output.connector, "mirror"))
        {
            let mirror = if label == "Extend workspace" {
                None
            } else {
                connected
                    .iter()
                    .find(|candidate| label == format!("Mirror {}", candidate.label))
                    .map(|candidate| candidate.connector.clone())
            };
            if mirror != cfg.mirror_of {
                cfg.mirror_of = mirror;
                changed = true;
            }
        }
        let exact = ui::doc::read_i64_coerced(state, root, output_key(&output.connector, "x"))
            .zip(ui::doc::read_i64_coerced(
                state,
                root,
                output_key(&output.connector, "y"),
            ))
            .and_then(|(x, y)| Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?)));
        if let Some((x, y)) = exact {
            if (cfg.x, cfg.y) != (x, y) {
                cfg.x = x;
                cfg.y = y;
                changed = true;
            }
        } else if let Some(position) =
            read_str_or_text(state, root, output_key(&output.connector, "position"))
        {
            let relative = ["Left", "Right", "Above", "Below"].iter().find_map(|side| {
                let label = position.strip_prefix(&format!("{side} of "))?;
                let target = connected
                    .iter()
                    .find(|candidate| candidate.label == label)?;
                Some((*side, target))
            });
            if let Some((side, reference)) = relative {
                let reference_cfg = next
                    .outputs
                    .get(&reference.connector)
                    .cloned()
                    .unwrap_or_else(|| reference.unconfigured());
                // Positions are logical, so neighbours are placed by their
                // logical extent (mode over scale, rotated), not their mode:
                // a 2x panel's right-hand neighbour starts 1280 in, not 2560.
                let (width, height) = logical_extent_of(output, cfg);
                let (reference_width, reference_height) =
                    logical_extent_of(reference, &reference_cfg);
                let point = match side {
                    "Left" => Some((reference_cfg.x.saturating_sub(width), reference_cfg.y)),
                    "Right" => Some((
                        reference_cfg.x.saturating_add(reference_width),
                        reference_cfg.y,
                    )),
                    "Above" => Some((reference_cfg.x, reference_cfg.y.saturating_sub(height))),
                    "Below" => Some((
                        reference_cfg.x,
                        reference_cfg.y.saturating_add(reference_height),
                    )),
                    _ => None,
                };
                if let Some((x, y)) = point
                    && (cfg.x, cfg.y) != (x, y)
                {
                    cfg.x = x;
                    cfg.y = y;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Which output should be primary this frame: a newly-toggled one wins,
    /// else keep whichever staged output is already both enabled and
    /// primary, else fall back to the first enabled output. Always writes
    /// the resolved `primary` key back so exactly one output reads true.
    fn apply_primary_selection(
        &self,
        state: &mut AutoCommit,
        root: &ObjId,
        connected: &[DisplayOutput],
        next: &mut DisplayConfig,
        changed: &mut bool,
    ) {
        let newly_selected_primary = connected.iter().find(|output| {
            read_bool(state, root, output_key(&output.connector, "primary")) == Some(true)
                && !self
                    .output_config
                    .outputs
                    .get(&output.connector)
                    .is_some_and(|config| config.primary)
        });
        let selected_primary = newly_selected_primary
            .map(|output| output.connector.as_str())
            .or_else(|| {
                connected.iter().find_map(|output| {
                    next.outputs
                        .get(&output.connector)
                        .filter(|config| config.enabled && config.primary)
                        .map(|_| output.connector.as_str())
                })
            })
            .or_else(|| {
                connected.iter().find_map(|output| {
                    next.outputs
                        .get(&output.connector)
                        .filter(|config| config.enabled)
                        .map(|_| output.connector.as_str())
                })
            });
        if let Some(selected) = selected_primary {
            for output in connected {
                let primary = output.connector == selected;
                if let Some(config) = next.outputs.get_mut(&output.connector)
                    && config.primary != primary
                {
                    config.primary = primary;
                    *changed = true;
                }
                let _ = state.put(root, output_key(&output.connector, "primary"), primary);
            }
        }
    }

    /// Mirroring is one viewport sampled by two sinks. Pick the best pixel
    /// extent both advertise, then each sink's best refresh for that extent.
    /// Merely sharing an origin with unrelated extents is not mirroring.
    fn apply_mirror_pairs(
        connected: &[DisplayOutput],
        next: &mut DisplayConfig,
        changed: &mut bool,
    ) {
        let mirror_pairs: Vec<(String, String)> = next
            .outputs
            .iter()
            .filter_map(|(connector, config)| {
                config
                    .mirror_of
                    .as_ref()
                    .map(|target| (connector.clone(), target.clone()))
            })
            .collect();
        for (connector, target) in mirror_pairs {
            let Some(source) = connected
                .iter()
                .find(|output| output.connector == connector)
            else {
                continue;
            };
            let Some(sink) = connected.iter().find(|output| output.connector == target) else {
                continue;
            };
            if let Some((source_mode, sink_mode)) = best_mirror_modes(source, sink) {
                if let Some(config) = next.outputs.get_mut(&connector)
                    && config.mode != Some(source_mode)
                {
                    config.mode = Some(source_mode);
                    *changed = true;
                }
                if let Some(config) = next.outputs.get_mut(&target)
                    && config.mode != Some(sink_mode)
                {
                    config.mode = Some(sink_mode);
                    *changed = true;
                }
            }
        }
    }

    /// Validate the staged config against real output capabilities and, if
    /// valid, make it the pending candidate the "Apply" button can request.
    fn stage_output_candidate(&mut self, connected: &[DisplayOutput], next: DisplayConfig) {
        let descriptors: Vec<_> = connected.iter().map(Self::output_descriptor).collect();
        match next.validate_for(&descriptors) {
            Ok(()) => self.pending_candidate = Some(next),
            Err(error) => {
                warn!(target: "tilo::settings", ?error, "rejected invalid display configuration");
            }
        }
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

    /// Apply/Keep/Revert button keys, plus reseeding controls once the
    /// compositor settles a transaction (confirmed, reverted, or rejected).
    fn sync_transaction_controls(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, APPLY_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, APPLY_DISPLAY_KEY, false);
            match self.pending_candidate.clone() {
                Some(candidate) => {
                    if let Err(error) = DisplayTransaction::request(self.display_set(), candidate) {
                        warn!(target: "tilo::settings", %error, "failed to stage display configuration");
                        self.finish_display_operation(Err(format!(
                            "Display configuration could not be staged: {error}"
                        )));
                    }
                }
                None => self.finish_display_operation(Err(
                    "There are no staged display changes to apply.".into(),
                )),
            }
        }
        if read_bool(state, root, KEEP_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, KEEP_DISPLAY_KEY, false);
            match DisplayTransaction::load() {
                Some(mut transaction) => {
                    if let Err(error) = transaction.request_confirmation() {
                        warn!(target: "tilo::settings", %error, "failed to confirm display configuration");
                        self.finish_display_operation(Err(format!(
                            "Display configuration could not be kept: {error}"
                        )));
                    }
                }
                None => self.finish_display_operation(Err(
                    "There is no display transaction to keep.".into(),
                )),
            }
        }
        if read_bool(state, root, REVERT_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, REVERT_DISPLAY_KEY, false);
            match DisplayTransaction::load() {
                Some(mut transaction) => {
                    if let Err(error) = transaction.request_revert() {
                        warn!(target: "tilo::settings", %error, "failed to revert display configuration");
                        self.finish_display_operation(Err(format!(
                            "Display configuration could not be reverted: {error}"
                        )));
                    }
                }
                None => self.finish_display_operation(Err(
                    "There is no display transaction to revert.".into(),
                )),
            }
        }
        if let Some(transaction) = DisplayTransaction::load() {
            let marker = (transaction.id, transaction.state);
            if self.last_seen_transaction != Some(marker) {
                self.last_seen_transaction = Some(marker);
                if matches!(
                    transaction.state,
                    DisplayTransactionState::Confirmed
                        | DisplayTransactionState::Reverted
                        | DisplayTransactionState::Rejected
                ) {
                    self.output_config = DisplayConfig::load_for(&self.display_set());
                    self.pending_candidate = None;
                    let committed = self.output_config.clone();
                    self.write_output_controls(state, root, &committed);
                    let result = match transaction.state {
                        DisplayTransactionState::Confirmed => Ok("Display changes kept.".into()),
                        DisplayTransactionState::Reverted => Ok("Display changes reverted.".into()),
                        DisplayTransactionState::Rejected => Err(transaction
                            .message
                            .unwrap_or_else(|| "Display configuration was rejected.".into())),
                        _ => return,
                    };
                    self.finish_display_operation(result);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    fn probed_pane() -> (DisplayPane, std::sync::mpsc::Sender<(f32, bool)>) {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        pane.hardware_probe = Some(probe_rx);
        (pane, probe_tx)
    }

    /// A brightness probe that lands after the pane was composed keeps the
    /// widgets the host's layout tree points into, and asks for a rebuild.
    #[test]
    fn late_hardware_probe_keeps_the_retained_tree_and_requests_a_rebuild() {
        let (mut pane, probe_tx) = probed_pane();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        pane.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let composed = |pane: &DisplayPane| {
            pane.children()
                .first()
                .map(|c| std::ptr::from_ref(*c).cast::<()>())
        };
        let before = composed(&pane);
        assert!(before.is_some(), "build_layout composes the pane");

        probe_tx
            .send((0.8, false))
            .expect("probe receiver is alive");
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            composed(&pane),
            before,
            "the host still holds layout pointers into this composition"
        );
        assert!(
            pane.take_structure_dirty(),
            "the landed probe requests a rebuild"
        );
        assert!(!pane.take_structure_dirty(), "and reports it once");
        let brightness = doc
            .get(&automerge::ROOT, BRIGHTNESS_KEY)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.as_f64());
        assert_eq!(brightness, Some(f64::from(0.8_f32)));

        let (pane, probe_tx) = probed_pane();
        let mut harness =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(pane), (900, 700));
        probe_tx
            .send((0.8, false))
            .expect("probe receiver is alive");
        harness.relayout();
        assert!(
            harness.last_relayout().rebuilt,
            "the headless host recomposes the pane after the probe lands"
        );
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
        let panel = || DisplayOutput {
            current_mode: mode(2560, 1600),
            ..classify_connector("card0-eDP-1", "connected")
        };
        let monitor = || DisplayOutput {
            current_mode: mode(1920, 1080),
            ..classify_connector("card0-HDMI-A-1", "connected")
        };
        let connected = [panel(), monitor()];
        let mut next = DisplayConfig::default();
        next.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                scale: 2.0,
                ..Default::default()
            },
        );
        let place = |label: &str, cfg: &mut OutputConfig| {
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, output_key("HDMI-A-1", "position"), label)
                .expect("put");
            DisplayPane::apply_output_placement_overrides(
                &doc,
                &automerge::ROOT,
                &monitor(),
                &connected,
                &next,
                cfg,
            )
        };

        // The 2x panel is 1280 logical pixels wide, so its right-hand
        // neighbour starts there — not at its 2560-pixel mode width.
        let mut cfg = OutputConfig::default();
        assert!(place("Right of Built-in display", &mut cfg));
        assert_eq!((cfg.x, cfg.y), (1280, 0));
        assert!(place("Below of Built-in display", &mut cfg));
        assert_eq!((cfg.x, cfg.y), (0, 800));

        // A monitor rotated to portrait is 1080 wide on the left.
        let mut portrait = OutputConfig {
            transform: OutputTransform::Rotate90,
            ..Default::default()
        };
        assert!(place("Left of Built-in display", &mut portrait));
        assert_eq!((portrait.x, portrait.y), (-1080, 0));
    }

    /// Builtin/external classification, distinct duplicate labels, migration and virtual connector filtering.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn display_connectors_are_classified_labelled_and_filtered() {
        // Case `classify_builtin_and_external_connectors`.
        {
            let builtin = classify_connector("card0-eDP-1", "connected\n");
            assert_eq!(builtin.label, "Built-in display");
            assert!(builtin.builtin);
            assert!(builtin.connected);

            let hdmi = classify_connector("card0-HDMI-A-1", "disconnected\n");
            assert_eq!(hdmi.label, "HDMI-A-1");
            assert!(!hdmi.builtin);
            assert!(!hdmi.connected);

            let dp = classify_connector("card1-DP-2", "connected");
            assert_eq!(dp.label, "DP-2");
            assert!(dp.connected);
        }

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
            assert_eq!(snapshot_label(&first, false), "HDMI-A-1 · pci:0000:01:00.0");
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
                "Dell U2720Q · HDMI-A-1 · pci:0000:01:00.0",
                "a monitor is named by its EDID and still told apart by port"
            );
            assert_eq!(
                snapshot_label(&second, false),
                "HDMI-A-1 · pci:0000:03:00.0"
            );

            let display = DisplayOutput {
                label: snapshot_label(&first, false),
                connector: first.connector.clone(),
                connector_label: first.label().to_string(),
                ..classify_connector("card0-HDMI-A-1", "connected")
            };
            let staged = DisplayPane::output_descriptor(&display);
            assert_eq!(staged.connector, "pci:0000:01:00.0/HDMI-A-1");
            assert_eq!(
                staged.connector_label, "HDMI-A-1",
                "the card qualifier is UI-only, never folded into descriptor data"
            );
        }

        // Case `duplicate_connector_migration_keeps_legacy_intent_without_cross_targeting`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let first = "pci:0000:01:00.0/HDMI-A-1";
            let second = "pci:0000:03:00.0/HDMI-A-1";
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                    .with_outputs(vec![
                        DisplayOutput {
                            label: "HDMI-A-1 · pci:0000:01:00.0".into(),
                            connector: first.into(),
                            ..classify_connector("card0-HDMI-A-1", "connected")
                        },
                        DisplayOutput {
                            label: "HDMI-A-1 · pci:0000:03:00.0".into(),
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

            let mut doc = AutoCommit::new();
            pane.seed_state(&mut doc, &automerge::ROOT);

            // The ambiguous old key remains intact for a future one-card
            // topology, but the current topology gets independent safe defaults
            // and independent CRDT/persistence keys.
            assert!(!pane.output_config.outputs["HDMI-A-1"].enabled);
            assert!(pane.output_config.outputs.contains_key(first));
            assert!(pane.output_config.outputs.contains_key(second));
            let persisted =
                serde_json::to_value(&pane.output_config).expect("display config serialises");
            let persisted_outputs = persisted
                .get("outputs")
                .and_then(serde_json::Value::as_object)
                .expect("outputs map");
            assert!(persisted_outputs.contains_key(first));
            assert!(persisted_outputs.contains_key(second));
            assert!(
                doc.get(&automerge::ROOT, output_key(first, "enabled"))
                    .unwrap()
                    .is_some()
            );
            assert!(
                doc.get(&automerge::ROOT, output_key(second, "enabled"))
                    .unwrap()
                    .is_some()
            );
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

    /// Brightness drags dispatch once, unchanged values rest, latches are inert, and seeding writes brightness.
    #[test]
    fn brightness_dispatches_only_local_changes() {
        // Case `brightness_slider_invokes_set_brightness_on_drag`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(rec.calls().is_empty(), "seed sync should not dispatch");
            let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
            pane.sync(&mut doc, &root);
            let calls = rec.calls();
            assert_eq!(calls.len(), 1, "expected one dispatch, got {calls:?}");
            assert!(calls[0].starts_with("set_brightness("));
            assert!(calls[0].contains("75."), "expected 75% in {:?}", calls[0]);
        }

        // Case `brightness_unchanged_does_not_redispatch`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            assert_eq!(rec.calls().len(), 1);
        }

        // Case `replicated_display_latches_are_inert_without_an_execution_token`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
            let commands = ui::LocalState::new(None);
            let mut pane =
                DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                    .with_command_state(&commands)
                    .with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let initial_heads = doc.get_heads();
            pane.sync(&mut doc, &root);
            assert_eq!(doc.get_heads(), initial_heads);
            let _ = doc.put(&root, BRIGHTNESS_KEY, 0.9);
            let _ = doc.put(&root, APPLY_DISPLAY_KEY, true);
            pane.sync(&mut doc, &root);

            assert!(rec.calls().is_empty());
            assert!((pane.last_brightness - 0.5).abs() < f32::EPSILON);
        }

        // Case `seed_writes_initial_brightness_into_crdt`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.42);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let (val, _) = doc.get(&root, BRIGHTNESS_KEY).unwrap().unwrap();
            match val {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::F64(f) => assert!((f - 0.42).abs() < 1e-6, "got {f}"),
                    other => panic!("expected F64 scalar, got {other:?}"),
                },
                automerge::Value::Object(obj_type) => {
                    panic!("expected scalar, got object {obj_type:?}")
                }
            }
        }
    }

    /// The arrangement follows injected, connected outputs and switches mirror/extend.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn display_arrangement_reflects_outputs_and_mirror_modes() {
        // Case `arrangement_reflects_injected_outputs`.
        // The arrangement card is driven by the injected output list, proving
        // it renders real hardware rather than a hard-coded pair.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                .with_outputs(vec![
                    DisplayOutput {
                        label: "Built-in display".into(),
                        connected: true,
                        builtin: true,
                        connector: "eDP-1".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-eDP-1", "connected")
                    },
                    DisplayOutput {
                        label: "HDMI-A-1".into(),
                        connected: true,
                        builtin: false,
                        connector: "HDMI-A-1".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-HDMI-A-1", "connected")
                    },
                ]);
            assert_eq!(pane.outputs().len(), 2);
            assert_eq!(pane.outputs()[1].label, "HDMI-A-1");
        }

        // Case `mirror_negotiation_allows_usb_c_monitor_refresh_to_differ`.
        {
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
        }

        // Case `settings_switches_usb_c_display_between_mirror_and_extend`.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let full_hd_60 = OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            };
            let full_hd_5994 = OutputMode {
                refresh_mhz: 59_940,
                ..full_hd_60
            };
            let outputs = vec![
                display_with_modes("eDP-1", "Built-in display", true, vec![full_hd_60]),
                display_with_modes("DP-1", "USB-C display", false, vec![full_hd_5994]),
            ];
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                    .with_outputs(outputs);
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

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(
                &root,
                output_key("DP-1", "mirror"),
                "Mirror Built-in display",
            );
            pane.sync(&mut doc, &root);
            let mirrored = pane.pending_candidate.clone().expect("mirror plan staged");
            assert_eq!(mirrored.outputs["DP-1"].mirror_of.as_deref(), Some("eDP-1"));
            assert_eq!(mirrored.outputs["DP-1"].mode, Some(full_hd_5994));
            assert_eq!(mirrored.outputs["eDP-1"].mode, Some(full_hd_60));

            // Simulate Keep, then select Extend. The saved independent origin was
            // never overwritten by mirroring and becomes active again.
            pane.output_config = mirrored;
            pane.pending_candidate = None;
            let _ = doc.put(&root, output_key("DP-1", "mirror"), "Extend workspace");
            pane.sync(&mut doc, &root);
            let extended = pane.pending_candidate.as_ref().expect("extend plan staged");
            assert_eq!(extended.outputs["DP-1"].mirror_of, None);
            assert_eq!(extended.outputs["DP-1"].x, 1920);
            assert_eq!(extended.outputs["DP-1"].y, 0);
        }

        // Case `arrangement_lists_only_connected_outputs`.
        // Only connected outputs become rows; a laptop's dozen idle DP ports are
        // summarised in a caption rather than enumerated one-per-row.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
                .with_outputs(vec![
                    DisplayOutput {
                        label: "Built-in display".into(),
                        connected: true,
                        builtin: true,
                        connector: "eDP-1".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-eDP-1", "connected")
                    },
                    DisplayOutput {
                        label: "DP-1".into(),
                        connected: false,
                        builtin: false,
                        connector: "DP-1".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-DP-1", "disconnected")
                    },
                    DisplayOutput {
                        label: "DP-2".into(),
                        connected: false,
                        builtin: false,
                        connector: "DP-2".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-DP-2", "disconnected")
                    },
                    DisplayOutput {
                        label: "HDMI-A-1".into(),
                        connected: false,
                        builtin: false,
                        connector: "HDMI-A-1".into(),
                        modes: Vec::new(),
                        hdr: false,
                        vrr: false,
                        color_profiles: Vec::new(),
                        ..classify_connector("card0-HDMI-A-1", "disconnected")
                    },
                ]);
            let shown = pane.connected_outputs();
            assert_eq!(
                shown.len(),
                1,
                "only the connected built-in display is listed"
            );
            assert_eq!(shown[0].label, "Built-in display");
        }
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
            color_profiles: Vec::new(),
            color_profile_labels: BTreeMap::new(),
            monitor: None,
        }
    }

    /// Scale labels, seeding, fractional persistence and staged display plans.
    #[test]
    fn display_scale_and_plans_persist_confirm_and_revert() {
        // Case `scale_label_and_factor_round_trip`.
        {
            for (label, factor) in DISPLAY_SCALES {
                assert_eq!(scale_label(*factor), *label);
                assert_eq!(scale_factor_of(label), Some(*factor));
            }
            // Nearest-label mapping for an off-grid stored value.
            assert_eq!(scale_label(1.48), "150%");
            assert_eq!(scale_factor_of("bogus"), None);
        }

        // Case `seed_writes_initial_scale_label_into_crdt`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            // with_dispatcher seeds initial_prefs = DisplayPrefs::default() (1.0).
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert_eq!(
                read_str_or_text(&doc, &root, DISPLAY_SCALE_KEY).as_deref(),
                Some("100%"),
                "seed should write the default 100% scale label"
            );
        }

        // Case `changing_scale_persists_fractional_factor_to_disk`.
        // Changing the scale dropdown's CRDT key drives the real persist path
        // and lands a fractional factor in the shared settings store.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root); // seed
            // User picks 150% in the dropdown.
            let _ = doc.put(&root, DISPLAY_SCALE_KEY, "150%");
            pane.sync(&mut doc, &root);
            let reloaded = SystemPrefs::load().display.scale;
            assert!(
                (reloaded - 1.5).abs() < 1e-9,
                "expected persisted display scale 1.5, got {reloaded}"
            );
        }

        // Case `display_plan_stages_confirms_and_reseeds_after_revert`.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane =
                DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
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

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let transform_key = output_key("eDP-1", "transform");
            let _ = doc.put(&root, &transform_key, "Portrait right");
            pane.sync(&mut doc, &root);
            assert_eq!(
                pane.pending_candidate.as_ref().unwrap().outputs["eDP-1"].transform,
                OutputTransform::Rotate90
            );
            assert_eq!(
                DisplayConfig::load_for(&set),
                committed,
                "editing is only staged"
            );

            let _ = doc.put(&root, APPLY_DISPLAY_KEY, true);
            pane.sync(&mut doc, &root);
            let mut transaction = DisplayTransaction::load().expect("Apply wrote a transaction");
            assert_eq!(transaction.state, DisplayTransactionState::Requested);
            assert_eq!(
                transaction.set, set,
                "the plan is staged for the monitors it was arranged on"
            );
            transaction.state = DisplayTransactionState::AwaitingConfirmation;
            transaction.deadline_unix_ms = Some(unix_now_ms() + 15_000);
            transaction.save().unwrap();
            let _ = doc.put(&root, KEEP_DISPLAY_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(
                DisplayTransaction::load().unwrap().state,
                DisplayTransactionState::ConfirmRequested
            );

            transaction.state = DisplayTransactionState::Reverted;
            transaction.deadline_unix_ms = None;
            transaction.save().unwrap();
            pane.sync(&mut doc, &root);
            assert!(pane.pending_candidate.is_none());
            assert_eq!(
                read_str_or_text(&doc, &root, &transform_key).as_deref(),
                Some("Landscape"),
                "revert resets controls to last known-good rather than reoffering the failed plan"
            );
        }
    }

    fn laptop_with_monitor_pane() -> DisplayPane {
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
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
            .with_outputs(vec![
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
        let pane = laptop_with_monitor_pane();
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
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
            .with_outputs(vec![
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
        let rec = Arc::new(RecordingDispatcher::default());
        let pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
            .with_outputs(vec![panel]);
        let connected = pane.connected_outputs();
        let displays = pane.arranged_displays(&connected);
        assert_eq!(displays[0].placement.width, 1280);
        assert_eq!(displays[0].placement.height, 800);

        let mut doc = AutoCommit::new();
        pane.write_output_controls(&mut doc, &automerge::ROOT, &pane.output_config);
        assert_eq!(
            read_str_or_text(&doc, &automerge::ROOT, output_key("eDP-1", "scale")).as_deref(),
            Some("200%"),
            "the scale control shows the scale the display runs at"
        );
    }

    #[test]
    fn a_dropped_display_stages_its_exact_position() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor_pane();
        let mut doc = AutoCommit::new();
        pane.execute_local_command(
            &mut doc,
            crate::actions::DisplayOperation::MoveOutput {
                connector: "HDMI-A-1".into(),
                x: -2560,
                y: -360,
            },
        );
        let staged = pane.pending_candidate.as_ref().expect("the drop is staged");
        assert_eq!(
            (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
            (-2560, -360)
        );
        assert!(
            DisplayTransaction::load().is_none(),
            "a drop is staged like every other edit, not applied"
        );

        // Choosing a relative position afterwards replaces the exact one.
        pane.execute_local_command(
            &mut doc,
            crate::actions::DisplayOperation::SetPosition {
                connector: "HDMI-A-1".into(),
                label: "Right of Built-in display".into(),
            },
        );
        let staged = pane.pending_candidate.as_ref().unwrap();
        assert_eq!(
            (staged.outputs["HDMI-A-1"].x, staged.outputs["HDMI-A-1"].y),
            (1920, 0)
        );
    }

    #[test]
    fn identify_asks_the_compositor_to_number_the_displays() {
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = laptop_with_monitor_pane();
        assert!(settings_store::display_identify::IdentifyRequest::load().is_none());
        let before = unix_now_ms();
        pane.execute_local_command(
            &mut AutoCommit::new(),
            crate::actions::DisplayOperation::Identify,
        );
        let after = unix_now_ms();
        let request = settings_store::display_identify::IdentifyRequest::load()
            .expect("Identify writes the request the compositor polls");
        // Judge the request against the moment it was issued, not against a
        // clock read after the command returned: a loaded machine can take
        // longer than the identify window just to run the command.
        assert!(
            (before..=after).contains(&request.requested_unix_ms),
            "the request is stamped with the time Identify ran"
        );
        assert_eq!(
            request.remaining_ms(request.requested_unix_ms),
            Some(settings_store::display_identify::IDENTIFY_MS),
            "a fresh request shows the numbers for the whole identify window"
        );
    }

    #[test]
    fn a_trusted_display_tool_is_listed_and_can_be_asked_again() {
        use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
        let _guard = crate::prefs::test_env::ConfigGuard::new();
        assert!(DisplayPane::build_trusted_tools_card().is_none());
        let kanshi = DisplayTool::new("/usr/bin/kanshi");
        TrustedDisplayTools::trust(kanshi.clone()).expect("trust");
        assert!(DisplayPane::build_trusted_tools_card().is_some());
        let section = crate::actions::displays_section(&AutoCommit::new(), &SystemPrefs::default());
        assert!(
            section.children.iter().any(|resource| {
                resource.id.0 == "settings.display-tool:/usr/bin/kanshi"
                    && resource.actions.iter().any(|action| action.id == "forget")
            }),
            "revoking is an action, not only a button"
        );

        let mut pane = laptop_with_monitor_pane();
        pane.execute_local_command(
            &mut AutoCommit::new(),
            crate::actions::DisplayOperation::ForgetDisplayTool {
                executable: kanshi.executable.clone(),
            },
        );
        assert!(!TrustedDisplayTools::load().contains(&kanshi));
        assert!(DisplayPane::build_trusted_tools_card().is_none());
    }

    #[test]
    fn an_external_monitor_card_sets_that_monitors_brightness() {
        let rec = Arc::new(RecordingDispatcher::default());
        let level = |connector: &str, level: f32| os_actions::MonitorLevel {
            connector: connector.into(),
            level,
        };
        let mut pane = DisplayPane::with_dispatcher(
            Arc::clone(&rec) as Arc<dyn Dispatcher + Send + Sync>,
            0.5,
        )
        .with_outputs(vec![
            display_with_modes("eDP-1", "Built-in display", true, Vec::new()),
            display_with_modes("DP-2", "Dell U2720Q · DP-2", false, Vec::new()),
            display_with_modes("DP-3", "LG 27GL850 · DP-3", false, Vec::new()),
        ])
        .with_monitor_brightness(&[level("eDP-1", 0.5), level("DP-2", 0.3)]);
        assert_eq!(
            pane.output_brightness.keys().collect::<Vec<_>>(),
            ["DP-2"],
            "the panel keeps the Brightness card; a monitor without DDC/CI gets no slider"
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, output_key("DP-2", "brightness"), 0.8);
        pane.sync(&mut doc, &root);
        assert!(
            rec.calls()
                .contains(&"set_display_brightness(DP-2, 80.00)".to_string()),
            "{:?}",
            rec.calls()
        );
    }
}
