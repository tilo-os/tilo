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
use std::time::{Duration, Instant, SystemTime};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Slider, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::prefs::{DisplayPrefs, SystemPrefs};
use crate::widgets::{El, caption, card, choice_row, rows, setting_row, value_row};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplayTransaction, DisplayTransactionState,
    OutputConfig, OutputDescriptor, OutputMode, OutputTransform,
};

/// CRDT key the brightness slider writes into.
pub const BRIGHTNESS_KEY: &str = "display.brightness";
/// CRDT key the auto-brightness toggle writes into.
pub const AUTO_BRIGHTNESS_KEY: &str = "display.auto_brightness";
/// CRDT key the night-light toggle writes into.
pub const NIGHT_LIGHT_KEY: &str = "display.night_light";
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

fn temperature_label(kelvin: u32) -> &'static str {
    NIGHT_LIGHT_TEMPERATURES
        .iter()
        .min_by_key(|(_, value)| value.abs_diff(kelvin))
        .map_or("Warm · 3700 K", |(label, _)| *label)
}

fn temperature_from_label(label: &str) -> Option<u32> {
    NIGHT_LIGHT_TEMPERATURES
        .iter()
        .find(|(candidate, _)| *candidate == label)
        .map(|(_, value)| *value)
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
fn scale_label(scale: f64) -> &'static str {
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

fn output_key(connector: &str, field: &str) -> String {
    format!("display.output.{connector}.{field}")
}

const TRANSFORMS: &[(&str, OutputTransform)] = &[
    ("Landscape", OutputTransform::Normal),
    ("Portrait right", OutputTransform::Rotate90),
    ("Upside down", OutputTransform::Rotate180),
    ("Portrait left", OutputTransform::Rotate270),
];

fn transform_label(transform: OutputTransform) -> &'static str {
    TRANSFORMS
        .iter()
        .find(|(_, candidate)| *candidate == transform)
        .map_or("Landscape", |(label, _)| *label)
}

fn transform_from_label(label: &str) -> Option<OutputTransform> {
    TRANSFORMS
        .iter()
        .find(|(candidate, _)| *candidate == label)
        .map(|(_, transform)| *transform)
}

/// One real display output, derived from a DRM connector.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub hdr: bool,
    pub sink_hdr: bool,
    pub hdr_unavailable_reason: Option<String>,
    pub vrr: bool,
    pub sink_vrr: bool,
    pub vrr_unavailable_reason: Option<String>,
    pub color_profiles: Vec<String>,
    pub color_profile_labels: BTreeMap<String, String>,
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
        hdr: false,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        vrr: false,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        color_profiles: Vec::new(),
        color_profile_labels: BTreeMap::new(),
    }
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
    let base = if builtin {
        "Built-in display"
    } else {
        descriptor.label()
    };
    descriptor
        .card_qualifier()
        .map_or_else(|| base.to_string(), |card| format!("{base} · {card}"))
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
                hdr: snapshot.descriptor.hdr,
                sink_hdr: snapshot.sink_hdr,
                hdr_unavailable_reason: snapshot.hdr_unavailable_reason,
                vrr: snapshot.descriptor.vrr,
                sink_vrr: snapshot.sink_vrr,
                vrr_unavailable_reason: snapshot.vrr_unavailable_reason,
                color_profiles: snapshot.descriptor.color_profiles,
                color_profile_labels: snapshot.color_profile_labels,
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
    pending_candidate: Option<DisplayConfig>,
    last_seen_transaction: Option<(u64, DisplayTransactionState)>,
    last_snapshot_check: Instant,
    snapshot_stamp: Option<SystemTime>,
    hardware_probe: Option<std::sync::mpsc::Receiver<(f32, bool)>>,
    brightness_tx: Option<std::sync::mpsc::Sender<f32>>,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
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
        let worker_dispatcher = Arc::clone(&dispatcher);
        std::thread::Builder::new()
            .name("settings-brightness-apply".into())
            .spawn(move || {
                while let Ok(percent) = brightness_rx.recv() {
                    if let Err(error) = worker_dispatcher.set_brightness(percent) {
                        tracing::warn!(target: "qos::settings", op = "set_brightness", %error, "display worker dispatch failed");
                    }
                }
            })
            .ok();
        Self {
            dispatcher,
            outputs: read_outputs(),
            ambient_sensor_available: false,
            initial_brightness: initial,
            initial_prefs: SystemPrefs::load().display,
            last_brightness: initial,
            last_prefs: DisplayPrefs::default(),
            output_config: DisplayConfig::load(),
            pending_candidate: None,
            last_seen_transaction: None,
            last_snapshot_check: Instant::now(),
            snapshot_stamp: settings_store::hardware::display_snapshot_path()
                .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
            hardware_probe: Some(probe_rx),
            brightness_tx: Some(brightness_tx),
            needs_seed: true,
            composed: None,
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
                hdr: false,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                vrr: false,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                color_profiles: Vec::new(),
                color_profile_labels: BTreeMap::new(),
            }],
            ambient_sensor_available: true,
            initial_brightness,
            initial_prefs: DisplayPrefs::default(),
            last_brightness: initial_brightness,
            last_prefs: DisplayPrefs::default(),
            output_config: DisplayConfig::default(),
            pending_candidate: None,
            last_seen_transaction: None,
            last_snapshot_check: Instant::now(),
            snapshot_stamp: None,
            hardware_probe: None,
            brightness_tx: None,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Pin the output list (tests / `--shots`).
    #[must_use]
    pub fn with_outputs(mut self, outputs: Vec<DisplayOutput>) -> Self {
        self.outputs = outputs;
        self
    }

    #[must_use]
    pub fn with_ambient_sensor(mut self, available: bool) -> Self {
        self.ambient_sensor_available = available;
        self
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
        cards.push(brightness);
        crate::widgets::cards(cards)
    }

    /// The "Displays" arrangement card: one row per *connected* output plus a
    /// caption about idle ports. See [`Self::connected_outputs`] for why
    /// disconnected ports aren't enumerated.
    fn build_arrangement_card(&self, connected: &[&DisplayOutput]) -> El {
        if connected.is_empty() {
            return card(
                "Displays",
                rows(vec![caption("No displays detected on this device.")]),
            );
        }
        let disconnected = self.outputs.len() - connected.len();
        let mut out_rows: Vec<El> = Vec::new();
        for (index, o) in connected.iter().enumerate() {
            let config = self.output_config.outputs.get(&o.connector);
            let status = if config.is_some_and(|config| !config.enabled) {
                "Disabled"
            } else if config.map_or(index == 0, |config| config.primary) {
                "Active · primary"
            } else {
                "Active"
            };
            out_rows.push(value_row(&o.label, status));
        }
        // One honest line about the idle ports rather than a row each.
        out_rows.push(caption(if disconnected == 0 {
            "Connect a second display to extend or mirror your screen."
        } else {
            "Connect a display to a free port to extend or mirror your screen."
        }));
        card("Displays", rows(out_rows))
    }

    /// The brightness slider + night-light comfort preferences card.
    fn build_brightness_card(&self) -> El {
        let automatic_brightness = if self.ambient_sensor_available {
            setting_row(
                "Automatic brightness",
                Box::new(
                    Toggle::new("settings.display.auto_brightness", AUTO_BRIGHTNESS_KEY)
                        .with_id("display.auto_brightness"),
                ),
            )
        } else {
            value_row("Automatic brightness", "No ambient-light sensor detected")
        };
        card(
            "Brightness",
            rows(vec![
                setting_row(
                    "Brightness",
                    Box::new(
                        Slider::new("settings.display.brightness", BRIGHTNESS_KEY)
                            .with_id("display.brightness"),
                    ),
                ),
                automatic_brightness,
                setting_row(
                    "Night light",
                    Box::new(
                        Toggle::new("settings.display.night_light", NIGHT_LIGHT_KEY)
                            .with_id("display.night_light"),
                    ),
                ),
                choice_row(
                    "Night light warmth",
                    Box::new(
                        Dropdown::new(
                            NIGHT_LIGHT_TEMPERATURES
                                .iter()
                                .map(|(label, _)| (*label).to_string())
                                .collect(),
                            NIGHT_LIGHT_TEMPERATURE_KEY,
                        )
                        .with_id("display.night_light_temperature")
                        .with_placeholder(temperature_label(
                            self.initial_prefs.night_light_temperature,
                        )),
                    ),
                ),
                caption(
                    "Night Light applies a whole-desktop colour transform. Automatic brightness follows the kernel ambient-light sensor with smoothing and a deadband.",
                ),
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
                ..Default::default()
            });
        let mut output_rows = Self::build_output_primary_rows(output, connected, &config);
        output_rows.extend(Self::build_output_mode_and_rotation_rows(output, &config));
        output_rows.extend(Self::build_output_capability_rows(output, &config));
        card(&output.label, rows(output_rows))
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
                "Use this display",
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.enabled", output.connector),
                        &enabled_key,
                    )
                    .with_id(&enabled_key),
                ),
            ),
            setting_row(
                "Primary display",
                Box::new(Toggle::new(&toggle_id, &primary_key).with_id(&primary_key)),
            ),
            choice_row(
                "Scale",
                Box::new(
                    Dropdown::new(
                        DISPLAY_SCALES.iter().map(|(l, _)| (*l).into()).collect(),
                        &scale_key,
                    )
                    .with_id(&scale_key)
                    .with_placeholder(scale_label(config.scale)),
                ),
            ),
            choice_row(
                "Position",
                Box::new(
                    Dropdown::new(
                        std::iter::once("Automatic".into())
                            .chain(
                                connected
                                    .iter()
                                    .filter(|candidate| candidate.connector != output.connector)
                                    .flat_map(|candidate| {
                                        ["Left", "Right", "Above", "Below"]
                                            .map(|side| format!("{side} of {}", candidate.label))
                                    }),
                            )
                            .collect(),
                        &position_key,
                    )
                    .with_id(&position_key)
                    .with_placeholder("Automatic"),
                ),
            ),
            choice_row(
                "Mirroring",
                Box::new(
                    Dropdown::new(
                        std::iter::once("Extend workspace".into())
                            .chain(
                                connected
                                    .iter()
                                    .filter(|candidate| candidate.connector != output.connector)
                                    .map(|candidate| format!("Mirror {}", candidate.label)),
                            )
                            .collect(),
                        &mirror_key,
                    )
                    .with_id(&mirror_key)
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
            let mode_placeholder = config
                .mode
                .or(output.current_mode)
                .map_or_else(|| "Preferred".into(), OutputMode::label);
            out_rows.push(choice_row(
                "Resolution & refresh rate",
                Box::new(
                    Dropdown::new(
                        output
                            .modes
                            .iter()
                            .copied()
                            .map(OutputMode::label)
                            .collect(),
                        &mode_key,
                    )
                    .with_id(&mode_key)
                    .with_placeholder(&mode_placeholder),
                ),
            ));
        }
        out_rows.push(choice_row(
            "Rotation",
            Box::new(
                Dropdown::new(
                    TRANSFORMS
                        .iter()
                        .map(|(label, _)| (*label).into())
                        .collect(),
                    &transform_key,
                )
                .with_id(&transform_key)
                .with_placeholder(transform_label(config.transform)),
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
            out_rows.push(setting_row(
                "HDR",
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.hdr", output.connector),
                        &hdr_key,
                    )
                    .with_id(&hdr_key),
                ),
            ));
        } else {
            out_rows.push(value_row(
                "HDR",
                output
                    .hdr_unavailable_reason
                    .as_deref()
                    .unwrap_or(if output.sink_hdr {
                        "Display support detected; compositor output support unavailable"
                    } else {
                        "Not supported by this display"
                    }),
            ));
        }
        if output.vrr {
            out_rows.push(setting_row(
                "Variable refresh rate",
                Box::new(
                    Toggle::new(
                        &format!("settings.display.{}.vrr", output.connector),
                        &vrr_key,
                    )
                    .with_id(&vrr_key),
                ),
            ));
        } else {
            out_rows.push(value_row(
                "Variable refresh rate",
                output
                    .vrr_unavailable_reason
                    .as_deref()
                    .unwrap_or(if output.sink_vrr {
                        "Display support detected; compositor output support unavailable"
                    } else {
                        "Not supported by this display"
                    }),
            ));
        }
        if output.color_profiles.is_empty() {
            out_rows.push(value_row(
                "Colour profile",
                "System default · no valid ICC profiles detected",
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
                "Colour profile",
                Box::new(
                    Dropdown::new(choices, &profile_key)
                        .with_id(&profile_key)
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
                    .map(|deadline| {
                        deadline
                            .saturating_sub(settings_store::hardware::unix_ms())
                            .div_ceil(1_000)
                    })
                    .unwrap_or_default();
                card(
                    "Keep this display configuration?",
                    rows(vec![
                        caption(&format!(
                            "Every display should be visible. Restoring the previous layout in {seconds} seconds unless you keep this one."
                        )),
                        setting_row(
                            "Everything looks correct",
                            Box::new(Button::new(
                                "settings.display.keep",
                                "Keep",
                                Some(KEEP_DISPLAY_KEY),
                            )),
                        ),
                        setting_row(
                            "Something is wrong",
                            Box::new(Button::new(
                                "settings.display.revert",
                                "Revert now",
                                Some(REVERT_DISPLAY_KEY),
                            )),
                        ),
                    ]),
                )
            }
            Some(
                DisplayTransactionState::Requested
                | DisplayTransactionState::ConfirmRequested
                | DisplayTransactionState::RevertRequested,
            ) => card(
                "Applying display configuration",
                rows(vec![caption(
                    transaction
                        .as_ref()
                        .and_then(|transaction| transaction.message.as_deref())
                        .unwrap_or("The compositor is validating and applying the output plan…"),
                )]),
            ),
            Some(DisplayTransactionState::Rejected) => {
                let mut content = vec![caption(
                    transaction
                        .as_ref()
                        .and_then(|transaction| transaction.message.as_deref())
                        .unwrap_or("The compositor rejected this output plan."),
                )];
                if self.pending_candidate.is_some() {
                    content.push(setting_row(
                        "Test the newly edited layout",
                        Box::new(Button::new(
                            "settings.display.apply",
                            "Try again",
                            Some(APPLY_DISPLAY_KEY),
                        )),
                    ));
                }
                card("Display change was not applied", rows(content))
            }
            _ if self.pending_candidate.is_some() => card(
                "Apply display changes",
                rows(vec![
                    caption(
                        "The layout is staged, not active. Applying it starts a 15-second safety test; closing Settings cannot cancel the automatic restore.",
                    ),
                    setting_row(
                        "Test staged layout",
                        Box::new(Button::new(
                            "settings.display.apply",
                            "Apply",
                            Some(APPLY_DISPLAY_KEY),
                        )),
                    ),
                ]),
            ),
            _ => card(
                "Display configuration",
                rows(vec![caption("No unapplied display changes.")]),
            ),
        }
    }

    /// Load prefs fresh, apply `f` to the display section, persist.
    fn persist_display(f: impl FnOnce(&mut DisplayPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.display);
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "display_prefs_save", error = %e, "failed to persist display prefs");
        }
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
                    ..Default::default()
                });
            let _ = state.put(root, output_key(&output.connector, "enabled"), cfg.enabled);
            let _ = state.put(root, output_key(&output.connector, "primary"), cfg.primary);
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.poll_hardware_probe(state, root);
        self.refresh_outputs_if_stale();
        if self.needs_seed {
            self.seed_state(state, root);
        }
        self.sync_brightness(state, root);
        self.sync_comfort_prefs(state, root);

        let connected: Vec<_> = self
            .outputs
            .iter()
            .filter(|o| o.connected)
            .cloned()
            .collect();
        self.sync_output_config(state, root, &connected);
        self.sync_transaction_controls(state, root);

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
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
                self.composed = None;
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
        self.last_snapshot_check = Instant::now();
        let stamp = settings_store::hardware::display_snapshot_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        if stamp != self.snapshot_stamp {
            self.snapshot_stamp = stamp;
            self.outputs = read_outputs();
            self.output_config = DisplayConfig::load();
            self.needs_seed = true;
            self.composed = None;
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
    fn sync_brightness(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, BRIGHTNESS_KEY)
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::F64(f) = s.as_ref()
        {
            let new = *f as f32;
            if (new - self.last_brightness).abs() > 0.001 {
                self.last_brightness = new;
                let percent = new.clamp(0.0, 1.0) * 100.0;
                if let Some(sender) = &self.brightness_tx {
                    let _ = sender.send(percent);
                } else if let Err(e) = self.dispatcher.set_brightness(percent) {
                    warn!(target: "qos::settings", op = "set_brightness", error = %e, "display pane dispatch failed");
                }
            }
        }
    }

    /// Comfort preferences → persist to the shared store on change.
    fn sync_comfort_prefs(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(b) = read_bool(state, root, AUTO_BRIGHTNESS_KEY)
            && b != self.last_prefs.auto_brightness
        {
            let effective = b && self.ambient_sensor_available;
            self.last_prefs.auto_brightness = effective;
            Self::persist_display(|d| d.auto_brightness = effective);
            if effective != b {
                let _ = state.put(root, AUTO_BRIGHTNESS_KEY, false);
            }
        }
        if let Some(b) = read_bool(state, root, NIGHT_LIGHT_KEY)
            && b != self.last_prefs.night_light
        {
            self.last_prefs.night_light = b;
            Self::persist_display(|d| d.night_light = b);
        }
        if let Some(label) = read_str(state, root, NIGHT_LIGHT_TEMPERATURE_KEY)
            && let Some(kelvin) = temperature_from_label(&label)
            && kelvin != self.last_prefs.night_light_temperature
        {
            self.last_prefs.night_light_temperature = kelvin;
            Self::persist_display(|d| d.night_light_temperature = kelvin);
        }
        // Display scale → persist to the shared store on change. The
        // compositor reads `display.scale` at session start and advertises
        // it as the preferred fractional scale.
        if let Some(label) = read_str(state, root, DISPLAY_SCALE_KEY)
            && let Some(v) = scale_factor_of(&label)
            && (v - self.last_prefs.scale).abs() > f64::EPSILON
        {
            self.last_prefs.scale = v;
            Self::persist_display(|d| d.scale = v);
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
                .unwrap_or_default();
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
        if let Some(enabled) = read_bool(state, root, &output_key(&output.connector, "enabled"))
            && enabled != cfg.enabled
        {
            cfg.enabled = enabled;
            if !enabled {
                cfg.primary = false;
            }
            changed = true;
        }
        if let Some(primary) = read_bool(state, root, &output_key(&output.connector, "primary"))
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
        if let Some(label) = read_str(state, root, &output_key(&output.connector, "scale"))
            && let Some(scale) = scale_factor_of(&label)
            && (scale - cfg.scale).abs() > f64::EPSILON
        {
            cfg.scale = scale;
            changed = true;
        }
        if let Some(label) = read_str(state, root, &output_key(&output.connector, "mode"))
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
        if let Some(label) = read_str(state, root, &output_key(&output.connector, "transform"))
            && let Some(transform) = transform_from_label(&label)
            && transform != cfg.transform
        {
            cfg.transform = transform;
            changed = true;
        }
        if output.hdr
            && let Some(enabled) = read_bool(state, root, &output_key(&output.connector, "hdr"))
            && enabled != cfg.hdr
        {
            cfg.hdr = enabled;
            changed = true;
        }
        if output.vrr
            && let Some(enabled) = read_bool(state, root, &output_key(&output.connector, "vrr"))
            && enabled != cfg.vrr
        {
            cfg.vrr = enabled;
            changed = true;
        }
        if let Some(label) = read_str(state, root, &output_key(&output.connector, "color_profile"))
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
        if let Some(label) = read_str(state, root, &output_key(&output.connector, "mirror")) {
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
        if let Some(position) = read_str(state, root, &output_key(&output.connector, "position")) {
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
                    .unwrap_or_default();
                let point = match side {
                    "Left" => Some((
                        reference_cfg.x
                            - output.current_mode.map_or(1920, |mode| mode.width) as i32,
                        reference_cfg.y,
                    )),
                    "Right" => Some((
                        reference_cfg.x
                            + reference.current_mode.map_or(1920, |mode| mode.width) as i32,
                        reference_cfg.y,
                    )),
                    "Above" => Some((
                        reference_cfg.x,
                        reference_cfg.y
                            - output.current_mode.map_or(1080, |mode| mode.height) as i32,
                    )),
                    "Below" => Some((
                        reference_cfg.x,
                        reference_cfg.y
                            + reference.current_mode.map_or(1080, |mode| mode.height) as i32,
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
            read_bool(state, root, &output_key(&output.connector, "primary")) == Some(true)
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

    /// Mirroring is one viewport sampled by two sinks. Pick the best mode
    /// both advertise and stage it on both connectors; merely sharing an
    /// origin with unrelated modes is not mirroring.
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
            let common = source
                .modes
                .iter()
                .filter(|mode| sink.modes.contains(mode))
                .copied()
                .max_by_key(|mode| {
                    (
                        u64::from(mode.width) * u64::from(mode.height),
                        mode.refresh_mhz,
                    )
                });
            if let Some(mode) = common {
                if let Some(config) = next.outputs.get_mut(&connector)
                    && config.mode != Some(mode)
                {
                    config.mode = Some(mode);
                    *changed = true;
                }
                if let Some(config) = next.outputs.get_mut(&target)
                    && config.mode != Some(mode)
                {
                    config.mode = Some(mode);
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
                warn!(target: "qos::settings", ?error, "rejected invalid display configuration");
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
        }
    }

    /// Apply/Keep/Revert button keys, plus reseeding controls once the
    /// compositor settles a transaction (confirmed, reverted, or rejected).
    fn sync_transaction_controls(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, APPLY_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, APPLY_DISPLAY_KEY, false);
            if let Some(candidate) = self.pending_candidate.clone()
                && let Err(error) = DisplayTransaction::request(candidate)
            {
                warn!(target: "qos::settings", %error, "failed to stage display configuration");
            }
        }
        if read_bool(state, root, KEEP_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, KEEP_DISPLAY_KEY, false);
            if let Some(mut transaction) = DisplayTransaction::load()
                && let Err(error) = transaction.request_confirmation()
            {
                warn!(target: "qos::settings", %error, "failed to confirm display configuration");
            }
        }
        if read_bool(state, root, REVERT_DISPLAY_KEY) == Some(true) {
            let _ = state.put(root, REVERT_DISPLAY_KEY, false);
            if let Some(mut transaction) = DisplayTransaction::load()
                && let Err(error) = transaction.request_revert()
            {
                warn!(target: "qos::settings", %error, "failed to revert display configuration");
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
                    self.output_config = DisplayConfig::load();
                    self.pending_candidate = None;
                    let committed = self.output_config.clone();
                    self.write_output_controls(state, root, &committed);
                }
            }
        }
    }
}

/// Read a `bool` scalar at `key`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

/// Read a string at `key` — handles both a scalar `Str` and an Automerge
/// `Text` object (a `Dropdown` may write either).
fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    #[test]
    fn classify_builtin_and_external_connectors() {
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

    #[test]
    fn qualified_duplicate_snapshot_labels_are_distinct_in_settings() {
        let first = OutputDescriptor {
            connector: "pci:0000:01:00.0/HDMI-A-1".into(),
            connector_label: "HDMI-A-1".into(),
            modes: vec![],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
        };
        let second = OutputDescriptor {
            connector: "pci:0000:03:00.0/HDMI-A-1".into(),
            connector_label: "HDMI-A-1".into(),
            modes: vec![],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
        };
        assert_eq!(snapshot_label(&first, false), "HDMI-A-1 · pci:0000:01:00.0");
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

    #[test]
    fn duplicate_connector_migration_keeps_legacy_intent_without_cross_targeting() {
        let rec = Arc::new(RecordingDispatcher::default());
        let first = "pci:0000:01:00.0/HDMI-A-1";
        let second = "pci:0000:03:00.0/HDMI-A-1";
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5)
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

    #[test]
    fn brightness_slider_invokes_set_brightness_on_drag() {
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

    #[test]
    fn brightness_unchanged_does_not_redispatch() {
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

    #[test]
    fn seed_writes_initial_brightness_into_crdt() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.42);
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

    /// The arrangement card is driven by the injected output list, proving
    /// it renders real hardware rather than a hard-coded pair.
    #[test]
    fn arrangement_reflects_injected_outputs() {
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

    /// The compositor's writeback connector and other virtual connectors are
    /// not displays a user plugs into, so they are filtered from the list.
    #[test]
    fn virtual_connectors_are_filtered() {
        assert!(is_virtual_connector("card0-Writeback-1"));
        assert!(is_virtual_connector("card0-Virtual-1"));
        assert!(!is_virtual_connector("card0-eDP-1"));
        assert!(!is_virtual_connector("card0-HDMI-A-1"));
        assert!(!is_virtual_connector("card0-DP-2"));
    }

    /// Only connected outputs become rows; a laptop's dozen idle DP ports are
    /// summarised in a caption rather than enumerated one-per-row.
    #[test]
    fn arrangement_lists_only_connected_outputs() {
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

    #[test]
    fn scale_label_and_factor_round_trip() {
        for (label, factor) in DISPLAY_SCALES {
            assert_eq!(scale_label(*factor), *label);
            assert_eq!(scale_factor_of(label), Some(*factor));
        }
        // Nearest-label mapping for an off-grid stored value.
        assert_eq!(scale_label(1.48), "150%");
        assert_eq!(scale_factor_of("bogus"), None);
    }

    #[test]
    fn seed_writes_initial_scale_label_into_crdt() {
        let rec = Arc::new(RecordingDispatcher::default());
        // with_dispatcher seeds initial_prefs = DisplayPrefs::default() (1.0).
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            read_str(&doc, &root, DISPLAY_SCALE_KEY).as_deref(),
            Some("100%"),
            "seed should write the default 100% scale label"
        );
    }

    /// Changing the scale dropdown's CRDT key drives the real persist path
    /// and lands a fractional factor in the shared settings store.
    #[test]
    fn changing_scale_persists_fractional_factor_to_disk() {
        let _guard = crate::prefs::test_env::ConfigGuard::new("display-scale");
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
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

    #[test]
    fn display_plan_stages_confirms_and_reseeds_after_revert() {
        let _guard = crate::prefs::test_env::ConfigGuard::new("display-transaction");
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = DisplayPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut committed = DisplayConfig::default();
        committed.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                mode: pane.outputs[0].current_mode,
                ..Default::default()
            },
        );
        committed.save().unwrap();
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
        assert_eq!(DisplayConfig::load(), committed, "editing is only staged");

        let _ = doc.put(&root, APPLY_DISPLAY_KEY, true);
        pane.sync(&mut doc, &root);
        let mut transaction = DisplayTransaction::load().expect("Apply wrote a transaction");
        assert_eq!(transaction.state, DisplayTransactionState::Requested);
        transaction.state = DisplayTransactionState::AwaitingConfirmation;
        transaction.deadline_unix_ms = Some(settings_store::hardware::unix_ms() + 15_000);
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
            read_str(&doc, &root, &transform_key).as_deref(),
            Some("Landscape"),
            "revert resets controls to last known-good rather than reoffering the failed plan"
        );
    }
}
