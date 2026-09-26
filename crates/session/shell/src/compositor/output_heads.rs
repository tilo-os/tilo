//! The display arrangement as external configuration tools see it.
//!
//! `wlr-randr`, `kanshi` and `way-displays` speak in *heads*: every physical
//! connector the compositor knows, enabled or not, with its modes, position,
//! transform, scale and adaptive-sync state. This module owns that view and
//! the translation of a tool's requested arrangement into a
//! [`DisplayConfig`] — the same plan Settings stages. The Wayland objects
//! that carry it live in [`super::output_management`]; everything here is
//! plain data so the mapping is covered without a display.

use std::collections::BTreeMap;

use settings_store::hardware::{
    DisplayConfig, DisplayConfigError, DisplayOutputSnapshot, OutputConfig, OutputDescriptor,
    OutputMode, OutputTransform,
};

use super::backend::{Output, OutputId};

/// One connector, as advertised to output-management clients.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputHead {
    /// What the connector can do. `descriptor.connector` is the head's name
    /// and the key its intent is stored under in [`DisplayConfig`].
    pub descriptor: OutputDescriptor,
    /// The live output driving this connector, when it is enabled.
    pub output: Option<OutputId>,
    pub description: String,
    pub physical_size_mm: Option<(u32, u32)>,
    pub current_mode: Option<OutputMode>,
    pub enabled: bool,
    /// Logical top-left — the coordinates [`OutputConfig::x`]/`y` use.
    pub position: (i32, i32),
    pub transform: OutputTransform,
    pub scale: f64,
    /// Only meaningful when `descriptor.vrr` says the sink supports it.
    pub adaptive_sync: bool,
}

impl OutputHead {
    pub fn name(&self) -> &str {
        &self.descriptor.connector
    }
}

/// Name a backend without connectors gives its `index`-th output.
pub fn virtual_head_name(id: OutputId) -> String {
    format!("VIRTUAL-{}", id.0.saturating_add(1))
}

/// Heads for a backend with no connector inventory (headless, nested): each
/// live output is one enabled head whose only mode is the one it runs.
pub fn heads_from_outputs(outputs: &[Output]) -> Vec<OutputHead> {
    outputs
        .iter()
        .map(|output| {
            let (width, height) = output.physical_size();
            let mode = OutputMode {
                width,
                height,
                refresh_mhz: output.refresh_mhz.unwrap_or(0),
            };
            let name = virtual_head_name(output.id);
            OutputHead {
                descriptor: OutputDescriptor {
                    connector: name.clone(),
                    connector_label: name,
                    modes: vec![mode],
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor: None,
                },
                output: Some(output.id),
                description: "Virtual display".into(),
                physical_size_mm: None,
                current_mode: Some(mode),
                enabled: true,
                position: output.logical_position,
                transform: output.transform,
                scale: f64::from(output.scale),
                adaptive_sync: false,
            }
        })
        .collect()
}

/// A connector the backend is driving right now.
#[derive(Clone, Debug)]
pub struct LiveHead {
    /// Collision-free connector key, matching `descriptor.connector`.
    pub key: String,
    pub output: Output,
    pub physical_size_mm: Option<(u32, u32)>,
}

/// Heads for a backend with a real connector inventory. `capabilities` is
/// every attached connector (the snapshot Settings reads), `live` the ones
/// currently driven, and `config` the effective intent that decides what a
/// disabled connector would come back as.
pub fn heads_from_capabilities(
    capabilities: &[DisplayOutputSnapshot],
    config: &DisplayConfig,
    live: &[LiveHead],
) -> Vec<OutputHead> {
    capabilities
        .iter()
        .map(|capability| {
            let key = capability.descriptor.connector.as_str();
            let intent = config.output_for(key).cloned().unwrap_or_default();
            let monitor = capability.descriptor.monitor.as_ref();
            let name = monitor
                .map(settings_store::hardware::MonitorIdentity::display_name)
                .filter(|name| !name.is_empty());
            let description = match (capability.builtin, name) {
                (true, _) => format!("Built-in display ({})", capability.descriptor.label()),
                (false, Some(name)) => format!("{name} ({})", capability.descriptor.label()),
                (false, None) => capability.descriptor.label().to_owned(),
            };
            let running = live.iter().find(|head| head.key == key);
            OutputHead {
                descriptor: capability.descriptor.clone(),
                output: running.map(|head| head.output.id),
                description,
                physical_size_mm: running
                    .and_then(|head| head.physical_size_mm)
                    .or_else(|| monitor.and_then(|m| m.physical_size_mm)),
                current_mode: Some(capability.current_mode),
                enabled: running.is_some(),
                position: running.map_or((intent.x, intent.y), |head| head.output.logical_position),
                transform: running.map_or(intent.transform, |head| head.output.transform),
                scale: running.map_or(intent.scale, |head| f64::from(head.output.scale)),
                adaptive_sync: capability.descriptor.vrr && intent.vrr,
            }
        })
        .collect()
}

/// A mode a client asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeRequest {
    /// One of the modes the head advertised.
    Advertised(OutputMode),
    /// `set_custom_mode`: honoured only when it names an advertised mode;
    /// Tilo does not synthesise timings. A zero refresh matches any rate.
    Custom {
        width: u32,
        height: u32,
        refresh_mhz: u32,
    },
}

/// The properties a client set on one enabled head. `None` keeps the
/// current value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadChanges {
    pub mode: Option<ModeRequest>,
    pub position: Option<(i32, i32)>,
    pub transform: Option<OutputTransform>,
    pub scale: Option<f64>,
    pub adaptive_sync: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HeadRequest {
    Enable(HeadChanges),
    Disable,
}

/// Why a requested arrangement cannot be applied.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrangementError {
    /// Every head must be either enabled or disabled by the configuration.
    UnconfiguredHead(String),
    /// The configuration names a head that no longer exists.
    UnknownHead(String),
    /// A custom mode that is not one the connector advertises.
    UnsupportedMode(String),
    /// The plan fails the same validation Settings' plans go through.
    Invalid(DisplayConfigError),
}

/// Translate a client's per-head requests into the display plan to stage.
///
/// `base` is the effective intent: entries for connectors the client did
/// not touch (unplugged docks, mirrors, colour profiles) survive unchanged,
/// so an external tool cannot silently erase what Settings stored.
///
/// # Errors
///
/// Returns the first [`ArrangementError`] the request hits.
pub fn plan_arrangement(
    base: &DisplayConfig,
    heads: &[OutputHead],
    requests: &BTreeMap<String, HeadRequest>,
) -> Result<DisplayConfig, ArrangementError> {
    if let Some(unknown) = requests
        .keys()
        .find(|key| !heads.iter().any(|head| head.name() == key.as_str()))
    {
        return Err(ArrangementError::UnknownHead(unknown.clone()));
    }
    let mut plan = base.clone();
    for head in heads {
        let key = head.name();
        let request = requests
            .get(key)
            .ok_or_else(|| ArrangementError::UnconfiguredHead(key.to_owned()))?;
        let entry = plan
            .outputs
            .entry(key.to_owned())
            .or_insert_with(|| OutputConfig {
                enabled: head.enabled,
                x: head.position.0,
                y: head.position.1,
                scale: head.scale,
                transform: head.transform,
                mode: head.current_mode,
                vrr: head.adaptive_sync,
                ..OutputConfig::default()
            });
        match request {
            HeadRequest::Disable => {
                entry.enabled = false;
                entry.primary = false;
            }
            HeadRequest::Enable(changes) => {
                entry.enabled = true;
                if let Some(mode) = changes.mode {
                    entry.mode = Some(resolve_mode(head, mode)?);
                }
                if let Some((x, y)) = changes.position {
                    entry.x = x;
                    entry.y = y;
                    // A head placed somewhere shows its own region.
                    entry.mirror_of = None;
                }
                if let Some(transform) = changes.transform {
                    entry.transform = transform;
                }
                if let Some(scale) = changes.scale {
                    entry.scale = scale;
                }
                if let Some(adaptive_sync) = changes.adaptive_sync {
                    entry.vrr = adaptive_sync;
                }
            }
        }
    }
    let descriptors: Vec<OutputDescriptor> =
        heads.iter().map(|head| head.descriptor.clone()).collect();
    plan.validate_for(&descriptors)
        .map_err(ArrangementError::Invalid)?;
    Ok(plan)
}

fn resolve_mode(head: &OutputHead, request: ModeRequest) -> Result<OutputMode, ArrangementError> {
    let found = match request {
        ModeRequest::Advertised(mode) => head.descriptor.modes.iter().find(|m| **m == mode),
        ModeRequest::Custom {
            width,
            height,
            refresh_mhz,
        } => head
            .descriptor
            .modes
            .iter()
            .filter(|m| m.width == width && m.height == height)
            .min_by_key(|m| {
                if refresh_mhz == 0 {
                    // Any rate will do; prefer the fastest.
                    u32::MAX.saturating_sub(m.refresh_mhz)
                } else {
                    m.refresh_mhz.abs_diff(refresh_mhz)
                }
            })
            .filter(|m| refresh_mhz == 0 || m.refresh_mhz.abs_diff(refresh_mhz) <= 500),
    };
    found
        .copied()
        .ok_or_else(|| ArrangementError::UnsupportedMode(head.name().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, refresh_mhz: u32) -> OutputMode {
        OutputMode {
            width,
            height,
            refresh_mhz,
        }
    }

    fn head(name: &str, modes: &[OutputMode], position: (i32, i32)) -> OutputHead {
        OutputHead {
            descriptor: OutputDescriptor {
                connector: name.into(),
                connector_label: name.into(),
                modes: modes.to_vec(),
                hdr: false,
                vrr: true,
                color_profiles: Vec::new(),
                monitor: None,
            },
            output: Some(OutputId(0)),
            description: name.into(),
            physical_size_mm: None,
            current_mode: modes.first().copied(),
            enabled: true,
            position,
            transform: OutputTransform::Normal,
            scale: 1.0,
            adaptive_sync: false,
        }
    }

    fn two_heads() -> Vec<OutputHead> {
        vec![
            head("eDP-1", &[mode(2560, 1600, 60_000)], (0, 0)),
            head(
                "DP-1",
                &[mode(2560, 1440, 144_000), mode(2560, 1440, 59_951)],
                (1280, 0),
            ),
        ]
    }

    #[test]
    fn every_head_must_be_configured() {
        let requests = BTreeMap::from([("eDP-1".to_owned(), HeadRequest::Disable)]);
        assert_eq!(
            plan_arrangement(&DisplayConfig::default(), &two_heads(), &requests),
            Err(ArrangementError::UnconfiguredHead("DP-1".into()))
        );
    }

    #[test]
    fn requested_properties_land_in_the_plan_and_untouched_ones_keep_their_values() {
        let requests = BTreeMap::from([
            (
                "eDP-1".to_owned(),
                HeadRequest::Enable(HeadChanges::default()),
            ),
            (
                "DP-1".to_owned(),
                HeadRequest::Enable(HeadChanges {
                    mode: Some(ModeRequest::Custom {
                        width: 2560,
                        height: 1440,
                        refresh_mhz: 60_000,
                    }),
                    position: Some((0, -1440)),
                    transform: Some(OutputTransform::Rotate90),
                    scale: Some(1.5),
                    adaptive_sync: Some(true),
                }),
            ),
        ]);
        let plan =
            plan_arrangement(&DisplayConfig::default(), &two_heads(), &requests).expect("valid");
        let panel = plan.output_for("eDP-1").expect("panel entry");
        assert!(panel.enabled);
        assert_eq!(panel.mode, Some(mode(2560, 1600, 60_000)));
        let monitor = plan.output_for("DP-1").expect("monitor entry");
        assert_eq!((monitor.x, monitor.y), (0, -1440));
        assert_eq!(monitor.mode, Some(mode(2560, 1440, 59_951)));
        assert_eq!(monitor.transform, OutputTransform::Rotate90);
        assert!((monitor.scale - 1.5).abs() < f64::EPSILON);
        assert!(monitor.vrr);
    }

    #[test]
    fn a_custom_mode_the_connector_cannot_drive_is_refused() {
        let requests = BTreeMap::from([
            (
                "eDP-1".to_owned(),
                HeadRequest::Enable(HeadChanges {
                    mode: Some(ModeRequest::Custom {
                        width: 1024,
                        height: 768,
                        refresh_mhz: 0,
                    }),
                    ..HeadChanges::default()
                }),
            ),
            ("DP-1".to_owned(), HeadRequest::Disable),
        ]);
        assert_eq!(
            plan_arrangement(&DisplayConfig::default(), &two_heads(), &requests),
            Err(ArrangementError::UnsupportedMode("eDP-1".into()))
        );
    }

    #[test]
    fn disabling_every_head_fails_the_settings_validation() {
        let requests = BTreeMap::from([
            ("eDP-1".to_owned(), HeadRequest::Disable),
            ("DP-1".to_owned(), HeadRequest::Disable),
        ]);
        assert_eq!(
            plan_arrangement(&DisplayConfig::default(), &two_heads(), &requests),
            Err(ArrangementError::Invalid(
                DisplayConfigError::NoEnabledOutput
            ))
        );
    }

    #[test]
    fn intent_for_unplugged_connectors_survives_an_external_apply() {
        let mut base = DisplayConfig::default();
        base.outputs.insert(
            "HDMI-A-1".into(),
            OutputConfig {
                x: 4000,
                ..OutputConfig::default()
            },
        );
        let requests = BTreeMap::from([
            (
                "eDP-1".to_owned(),
                HeadRequest::Enable(HeadChanges::default()),
            ),
            ("DP-1".to_owned(), HeadRequest::Disable),
        ]);
        let plan = plan_arrangement(&base, &two_heads(), &requests).expect("valid");
        assert_eq!(plan.output_for("HDMI-A-1").map(|o| o.x), Some(4000));
        assert_eq!(plan.output_for("DP-1").map(|o| o.enabled), Some(false));
    }

    #[test]
    fn disabled_connectors_report_their_stored_intent() {
        let capability = |connector: &str| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode(1920, 1080, 60_000)],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: mode(1920, 1080, 60_000),
            builtin: connector == "eDP-1",
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        };
        let mut config = DisplayConfig::default();
        config.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                enabled: false,
                x: 1920,
                y: 0,
                ..OutputConfig::default()
            },
        );
        let mut output = Output::new(OutputId(3), (1920, 1080), 1.0);
        output.logical_position = (0, 0);
        let live = [LiveHead {
            key: "eDP-1".into(),
            output,
            physical_size_mm: Some((300, 190)),
        }];
        let heads =
            heads_from_capabilities(&[capability("eDP-1"), capability("DP-1")], &config, &live);
        let panel = heads.first().expect("panel");
        assert!(panel.enabled);
        assert_eq!(panel.output, Some(OutputId(3)));
        assert_eq!(panel.description, "Built-in display (eDP-1)");
        assert_eq!(panel.physical_size_mm, Some((300, 190)));
        let monitor = heads.get(1).expect("monitor");
        assert!(!monitor.enabled);
        assert_eq!(monitor.output, None);
        assert_eq!(monitor.position, (1920, 0));
    }
}
