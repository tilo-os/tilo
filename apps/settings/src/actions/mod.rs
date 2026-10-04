//! The hand-built resource rows of the Settings sections that drive this
//! device's hardware and services, and the typed local-operation protocol
//! their controls and the palette share: the visible pane owns the
//! privileged command and the palette uses the same bounded
//! request/lifecycle, without making a device action replay on a peer.
//!
//! Preferences are not here: each is a [`crate::effects::SettingsEffect`]
//! that writes the person's or the device's settings record.

#![allow(
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::redundant_locals,
    clippy::too_many_lines,
    clippy::unreachable,
    reason = "resource builders operate on closed, locally-constructed action schemas; fallible public resolution remains bounded and returns Option/Result"
)]

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    ActionAddress, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};

use crate::categories::Category;
use crate::prefs::SystemPrefs;

/// Operation protocol used by every Settings pane's exported presentation
/// action. The payload is the category's stable slug.
pub const OPEN_PANE_OPERATION_TYPE: &str = "settings.open-pane";

/// Stable action id shared by every pane resource.
pub const OPEN_PANE_ACTION: &str = "open";
/// Hardware-network commands. They intentionally address the current
/// machine's radios and diagnostics, rather than a replicated preference.
pub const NETWORK_OPERATION_TYPE: &str = "settings.network-operation";
/// Compositor-backed output configuration commands. These stage or confirm a
/// transaction for the current machine; they never become shared preferences.
pub const DISPLAY_OPERATION_TYPE: &str = "settings.display-operation";

/// Collaboration policy for every action exported by Settings. There is no
/// implicit/default bucket: a new operation type must be added here before it
/// can be published, so hardware effects cannot accidentally become replayable
/// document changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingScope {
    PortableDocument,
    DeviceLocalEffect,
    Presentation,
}

pub(crate) const DEVICE_LOCAL_OPERATION_TYPES: &[&str] =
    &[NETWORK_OPERATION_TYPE, DISPLAY_OPERATION_TYPE];

pub fn classify_action(spec: &ActionSpec) -> Option<SettingScope> {
    match spec.kind {
        OperationKind::Document => Some(SettingScope::PortableDocument),
        OperationKind::Effect
            if spec
                .operation_type
                .as_deref()
                .is_some_and(|type_id| DEVICE_LOCAL_OPERATION_TYPES.contains(&type_id)) =>
        {
            Some(SettingScope::DeviceLocalEffect)
        }
        OperationKind::View => Some(SettingScope::Presentation),
        OperationKind::Effect => None,
    }
}

/// Hard ceiling for any live/discovered collection published by Settings.
///
/// A compositor, service, or corrupted local inventory is not allowed to make
/// opening global search allocate an unbounded resource/action tree. Exact
/// lookup is scoped to one section below and uses the same ceiling.
const MAX_DYNAMIC_RESOURCES: usize = 256;
/// Hard ceiling for choices/actions attached to one dynamic resource.
const MAX_DYNAMIC_CHOICES: usize = 128;

/// Bounded commands owned by the Network pane. Radio state and diagnostics
/// belong to the current device and must never replay on another machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkOperation {
    Refresh,
    SetWifiEnabled { enabled: bool },
    SetWiredEnabled { enabled: bool },
    SetAirplaneMode { enabled: bool },
    SetHotspot { enabled: bool },
    SetMobileBroadband { enabled: bool },
    RunDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DisplayOperation {
    SetBrightness {
        value: f64,
    },
    SetAutoBrightness {
        enabled: bool,
    },
    SetNightLight {
        enabled: bool,
    },
    SetNightLightSchedule {
        enabled: bool,
    },
    SetNightLightTemperature {
        label: String,
    },
    SetScale {
        label: String,
    },
    SetPrimary {
        connector: String,
        primary: bool,
    },
    SetOutputScale {
        connector: String,
        label: String,
    },
    SetPosition {
        connector: String,
        label: String,
    },
    SetMirror {
        connector: String,
        label: String,
    },
    SetModeLabel {
        connector: String,
        label: String,
    },
    SetTransformLabel {
        connector: String,
        label: String,
    },
    SetColorProfileLabel {
        connector: String,
        label: String,
    },
    /// One monitor's brightness, `0.0..=1.0`: the external monitor on
    /// `connector`, over DDC/CI.
    SetOutputBrightness {
        connector: String,
        value: f64,
    },
    SetEnabled {
        connector: String,
        enabled: bool,
    },
    SetMode {
        connector: String,
        mode: settings_store::hardware::OutputMode,
    },
    SetTransform {
        connector: String,
        transform: settings_store::hardware::OutputTransform,
    },
    SetHdr {
        connector: String,
        enabled: bool,
    },
    /// How bright SDR content is while `connector` runs in HDR, as a
    /// fraction `0.0..=1.0` of the offered range.
    SetSdrBrightness {
        connector: String,
        value: f64,
    },
    SetVrr {
        connector: String,
        enabled: bool,
    },
    SetColorProfile {
        connector: String,
        profile: Option<String>,
    },
    /// Place an output's top-left corner at an exact point of the shared
    /// desktop, as dragging it in the arrangement canvas does.
    MoveOutput {
        connector: String,
        x: i32,
        y: i32,
    },
    /// Paint every display's number and name on it for a few seconds.
    Identify,
    Apply,
    Confirm,
    Revert,
    /// Ask again before keeping display changes from this layout tool.
    ForgetDisplayTool {
        executable: String,
    },
}

/// Process-local delivery envelope for device effects. This value is never
/// serialized into Automerge: the collaborative document contains portable
/// preferences only, while the selected pane consumes one typed command.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalPaneCommand {
    Network(NetworkOperation),
    Display(DisplayOperation),
}

/// Lifecycle reported by the pane that actually executes a device-local
/// command. Unlike a document action this is intentionally process-local and
/// never replicated or persisted as a preference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LocalOperationStatus {
    #[default]
    Idle,
    Running {
        token: String,
    },
    Succeeded {
        token: String,
        message: String,
    },
    Failed {
        token: String,
        message: String,
    },
}

/// Stable local lifecycle token. It is deliberately separate from the wire
/// payload: panes compare only the command they consumed, so an unrelated UI
/// action can never complete an agent's pending invocation.
///
/// # Errors
///
/// Returns an error when the type id or serialized payload is not a supported
/// device-local Settings operation.
pub fn local_operation_token(type_id: &str, payload: &[u8]) -> Result<String, String> {
    match type_id {
        NETWORK_OPERATION_TYPE => {
            let operation: NetworkOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        DISPLAY_OPERATION_TYPE => {
            let operation: DisplayOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        _ => Err("unknown device-local operation type".into()),
    }
}

pub fn local_operation_token_for<T: Serialize>(type_id: &str, operation: &T) -> String {
    format!(
        "{type_id}:{}",
        serde_json::to_string(operation).unwrap_or_default()
    )
}

/// Stable resource identity for a Settings pane.
pub fn pane_resource_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

/// Canonical address used by search, agents, accessibility, and resource-tree
/// navigation to reveal exactly one Settings pane.
pub fn open_pane_address(category: Category) -> ActionAddress {
    ActionAddress::new(pane_resource_id(category), OPEN_PANE_ACTION)
}

/// Build the minimal, canonical resource needed to resolve a Settings search
/// result lazily.  Search navigation is deliberately independent from the
/// eager resource snapshot: opening a category must remain available even
/// while a dynamic pane is loading or its device inventory is unavailable.
///
/// Keep this constructor next to [`open_pane_address`] so the address payload,
/// action id and resource id cannot drift apart between the search provider
/// and the full resource tree.
pub fn pane_navigation_resource(category: Category) -> Resource {
    let mut pane = Resource::new(
        pane_resource_id(category),
        "settings.section",
        category.label(),
    );
    if let Ok(open) = ActionSpec::operation(
        OPEN_PANE_ACTION,
        format!("Open {}", category.label()),
        OperationKind::View,
        OPEN_PANE_OPERATION_TYPE,
        &category.slug(),
    ) {
        pane = pane.with_action(open);
    }
    pane
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// controls (see `crate::panes`) and the Ctrl+K resource tree below.
// ===========================================================================

// ===========================================================================
// Reading the current document value (presentation side — builds labels and
// flip targets; never part of `apply`).
// ===========================================================================

// ===========================================================================
// Resource tree
// ===========================================================================

/// Contract for commands that are meaningful only on the current device.  It
/// makes the non-collaborative locus explicit in palette, history and agent
/// clients without pretending a completed local command is a replicated
/// preference change.
fn local_operation_contract(tags: &[&str]) -> ActionContract {
    ActionContract {
        // The on-screen pane performs the privileged work after admission, so
        // a resource invocation is a tracked background operation, not an
        // immediate document write. It cannot be stopped safely once a system
        // command has started.
        retry: RetryPolicy::AtMostOnce,
        // No device-local operation publishes a captured inverse action. A
        // person may be able to make a new compensating choice later, but
        // that is not an undo contract.
        reversible: false,
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "string" }),
        },
        tags: tags.iter().map(|tag| (*tag).into()).collect(),
        ..ActionContract::default()
    }
}

fn local_operation_spec<T: Serialize>(
    id: impl Into<String>,
    label: impl Into<String>,
    kind: OperationKind,
    type_id: &str,
    operation: &T,
    tags: &[&str],
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, type_id, operation)
        .ok()
        .map(|spec| spec.with_contract(local_operation_contract(tags)))
}

fn local_bool_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(bool) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "On or off on this device",
        OperationKind::Effect,
        type_id,
        vec![ActionParam::required(
            "value",
            ParamType::Boolean,
            "Enabled",
        )],
        move |args| build(args.boolean("value").unwrap_or(false)),
    )
    .with_contract(local_operation_contract(tags))
}

fn local_number_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(f64) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "A value from 0 to 1",
        OperationKind::Effect,
        type_id,
        vec![
            ActionParam::required("value", ParamType::Number, "Value")
                .with_schema(serde_json::json!({ "minimum": 0.0, "maximum": 1.0 })),
        ],
        move |args| build(args.number("value").unwrap_or(0.0)),
    )
    .with_contract(local_operation_contract(tags))
}

fn local_choice_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    choices: Vec<String>,
    tags: &'static [&'static str],
    build: impl Fn(String) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "Choose a value available on this device",
        OperationKind::Effect,
        type_id,
        vec![ActionParam::required("choice", ParamType::Text, "Choice").with_choices(choices)],
        move |args| build(args.text("choice").unwrap_or_default().to_string()),
    )
    .with_contract(local_operation_contract(tags))
}

/// Largest desktop coordinate the arrangement accepts; far beyond any real
/// multi-display desk, small enough that every sum stays in range.
pub const MAX_DISPLAY_COORDINATE: i32 = 65_536;

fn local_point_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(i32, i32) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    let bound = i64::from(MAX_DISPLAY_COORDINATE);
    let coordinate = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Integer, description)
            .with_schema(serde_json::json!({ "minimum": -bound, "maximum": bound }))
    };
    ActionSpec::typed_operation(
        id,
        label,
        "A point on the shared desktop, in desktop pixels",
        OperationKind::Effect,
        type_id,
        vec![coordinate("x", "Left edge"), coordinate("y", "Top edge")],
        move |args| {
            let axis = |name: &str| {
                i32::try_from(args.integer(name).unwrap_or(0).clamp(-bound, bound))
                    .unwrap_or_default()
            };
            build(axis("x"), axis("y"))
        },
    )
    .with_contract(local_operation_contract(tags))
}

fn encode_local_operation<T: Serialize>(type_id: &str, operation: &T) -> EncodedAction {
    EncodedAction::encode_operation(type_id, operation).unwrap_or_else(|_| EncodedAction {
        type_id: type_id.into(),
        payload: Vec::new(),
    })
}

/// The sections of the device's hardware and services and the account
/// inventory, each with the local operations its controls use.
pub(crate) fn live_device_resources(doc: &AutoCommit) -> Vec<Resource> {
    let prefs = SystemPrefs::load();
    Category::ALL
        .iter()
        .copied()
        .map(|category| section_resource(category, doc, &prefs))
        .collect()
}

/// The device sections under one root, as a test reads them.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("settings.root", "settings.root", "Settings");
    for section in live_device_resources(doc) {
        root = root.with_child(section);
    }
    vec![root]
}

fn section_resource(category: Category, doc: &AutoCommit, prefs: &SystemPrefs) -> Resource {
    let mut pane = match category {
        Category::Network => network_section(doc),
        Category::Displays => displays_section(doc, prefs),
        _ => Resource::new(
            pane_resource_id(category),
            "settings.section",
            category.label(),
        ),
    };
    if let Some(open) = pane_navigation_resource(category)
        .actions
        .into_iter()
        .next()
    {
        pane.actions.insert(0, open);
    }
    pane
}

fn category_for_resource_id(id: &str) -> Option<Category> {
    if let Some(category) = Category::ALL
        .iter()
        .copied()
        .find(|category| pane_resource_id(*category) == id)
    {
        return Some(category);
    }
    let mappings = [
        ("settings.display-output:", Category::Displays),
        ("settings.input-source:", Category::MouseKeyboard),
    ];
    mappings
        .into_iter()
        .find_map(|(prefix, category)| id.starts_with(prefix).then_some(category))
}

fn find_resource(mut resource: Resource, id: &str) -> Option<Resource> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .drain(..)
        .find_map(|child| find_resource(child, id))
}

#[allow(
    clippy::too_many_lines,
    reason = "resource tree mirrors display capability hierarchy"
)]
pub(crate) fn displays_section(_doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    let mut sec = Resource::new(
        pane_resource_id(Category::Displays),
        "settings.section",
        "Displays",
    );
    sec = sec.with_action(local_bool_spec(
        "auto_brightness",
        "Set automatic brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetAutoBrightness { enabled },
            )
        },
    ));
    sec = sec.with_action(local_bool_spec(
        "night_light",
        "Set Night Light",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLight { enabled },
            )
        },
    ));
    sec = sec.with_action(local_bool_spec(
        "night_light_schedule",
        "Set Night Light schedule",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLightSchedule { enabled },
            )
        },
    ));
    sec = sec.with_action(local_choice_spec(
        "night_light_temperature",
        "Set Night Light warmth",
        DISPLAY_OPERATION_TYPE,
        vec!["Subtle · 4500 K", "Warm · 3700 K", "Very warm · 2800 K"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLightTemperature { label },
            )
        },
    ));
    sec = sec.with_action(local_number_spec(
        "brightness",
        "Set brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetBrightness { value },
            )
        },
    ));
    // The Display pane persists its scale as the same human-readable labels
    // shown in its picker (for example `150%`), not as a raw factor.  Keeping
    // the resource action on that wire format means Ctrl+K, agents and the
    // visible dropdown all reach the pane through one typed value contract.
    sec = sec.with_action(local_choice_spec(
        "scale",
        "Set display scale",
        DISPLAY_OPERATION_TYPE,
        vec!["100%", "125%", "150%", "175%", "200%"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetScale { label },
            )
        },
    ));
    // Output topology is supplied by the compositor's live, PID-validated
    // snapshot.  Do not turn an absent/stale snapshot into invented monitors.
    if let Some(snapshot) = settings_store::hardware::DisplayHardwareSnapshot::load() {
        let output_labels: Vec<_> = snapshot
            .outputs
            .iter()
            .take(MAX_DYNAMIC_RESOURCES)
            .map(|output| {
                (
                    output.descriptor.connector.clone(),
                    output.descriptor.label().to_string(),
                )
            })
            .collect();
        for output in snapshot.outputs.into_iter().take(MAX_DYNAMIC_RESOURCES) {
            let descriptor = output.descriptor;
            let connector = descriptor.connector.clone();
            let mut resource = Resource::new(
                format!("settings.display-output:{connector}"),
                "settings.display-output",
                descriptor.label().to_string(),
            );
            resource = resource.with_action(output_brightness_spec(connector.clone()));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_bool_spec(
                "enabled",
                "Set display enabled",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |enabled| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetEnabled {
                            connector: operation_connector.clone(),
                            enabled,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "scale",
                "Set output scale",
                DISPLAY_OPERATION_TYPE,
                vec!["100%", "125%", "150%", "175%", "200%"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetOutputScale {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let position_choices: Vec<String> = std::iter::once("Automatic".into())
                .chain(
                    output_labels
                        .iter()
                        .filter(|(candidate, _)| candidate != &connector)
                        .flat_map(|(_, label)| {
                            ["Left", "Right", "Above", "Below"]
                                .map(|side| format!("{side} of {label}"))
                        }),
                )
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "position",
                "Set output position",
                DISPLAY_OPERATION_TYPE,
                position_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetPosition {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_point_spec(
                "move",
                "Move display",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |x, y| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::MoveOutput {
                            connector: operation_connector.clone(),
                            x,
                            y,
                        },
                    )
                },
            ));
            let mirror_choices: Vec<String> = std::iter::once("Extend workspace".into())
                .chain(
                    output_labels
                        .iter()
                        .filter(|(candidate, _)| candidate != &connector)
                        .map(|(_, label)| format!("Mirror {label}")),
                )
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "mirror",
                "Set output mirroring",
                DISPLAY_OPERATION_TYPE,
                mirror_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetMirror {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_bool_spec(
                "primary",
                "Set primary display",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |primary| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetPrimary {
                            connector: operation_connector.clone(),
                            primary,
                        },
                    )
                },
            ));
            let mode_choices = descriptor
                .modes
                .iter()
                .take(MAX_DYNAMIC_CHOICES)
                .map(|mode| mode.label())
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "mode",
                "Set resolution and refresh rate",
                DISPLAY_OPERATION_TYPE,
                mode_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetModeLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "transform",
                "Set display orientation",
                DISPLAY_OPERATION_TYPE,
                vec![
                    "Landscape",
                    "Portrait right",
                    "Upside down",
                    "Portrait left",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetTransformLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            if descriptor.hdr {
                resource = resource.with_action(sdr_brightness_spec(connector.clone()));
                let operation_connector = connector.clone();
                resource = resource.with_action(local_bool_spec(
                    "hdr",
                    "Set HDR",
                    DISPLAY_OPERATION_TYPE,
                    &["device-local", "display", "live-output"],
                    move |enabled| {
                        encode_local_operation(
                            DISPLAY_OPERATION_TYPE,
                            &DisplayOperation::SetHdr {
                                connector: operation_connector.clone(),
                                enabled,
                            },
                        )
                    },
                ));
            }
            if descriptor.vrr {
                let operation_connector = connector.clone();
                resource = resource.with_action(local_bool_spec(
                    "vrr",
                    "Set variable refresh rate",
                    DISPLAY_OPERATION_TYPE,
                    &["device-local", "display", "live-output"],
                    move |enabled| {
                        encode_local_operation(
                            DISPLAY_OPERATION_TYPE,
                            &DisplayOperation::SetVrr {
                                connector: operation_connector.clone(),
                                enabled,
                            },
                        )
                    },
                ));
            }
            let mut profile_choices = vec!["System default".to_string()];
            profile_choices.extend(
                descriptor
                    .color_profiles
                    .iter()
                    .take(MAX_DYNAMIC_CHOICES - 1)
                    .filter_map(|id| {
                        output
                            .color_profile_labels
                            .get(id)
                            .map(|name| format!("{name} · {id}"))
                    }),
            );
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "color-profile",
                "Set colour profile",
                DISPLAY_OPERATION_TYPE,
                profile_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetColorProfileLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            sec = sec.with_child(resource);
        }
    }
    for (id, label, operation) in [
        (
            "apply",
            "Apply staged display changes",
            DisplayOperation::Apply,
        ),
        ("confirm", "Keep display changes", DisplayOperation::Confirm),
        ("revert", "Revert display changes", DisplayOperation::Revert),
        (
            "identify",
            "Show each display's number on it",
            DisplayOperation::Identify,
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            DISPLAY_OPERATION_TYPE,
            &operation,
            &["device-local", "display", "transaction"],
        ) {
            sec = sec.with_action(action);
        }
    }
    for tool in settings_store::display_tools::TrustedDisplayTools::load()
        .tools
        .iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        sec = sec.with_child(display_tool_resource(tool));
    }
    sec
}

/// The resource for one trusted display tool, carrying its `forget` action.
fn display_tool_resource(tool: &settings_store::display_tools::DisplayTool) -> Resource {
    let resource = Resource::new(
        format!("settings.display-tool:{}", tool.executable),
        "settings.display-tool",
        tool.name().to_string(),
    );
    match local_operation_spec(
        "forget",
        "Ask before keeping this tool's display changes",
        OperationKind::Effect,
        DISPLAY_OPERATION_TYPE,
        &DisplayOperation::ForgetDisplayTool {
            executable: tool.executable.clone(),
        },
        &["device-local", "display", "trusted-tool"],
    ) {
        Some(action) => resource.with_action(action),
        None => resource,
    }
}

/// Binding for a trusted tool's "Ask again" button: the same `forget`
/// action an agent or script invokes.
pub fn display_tool_forget_binding(
    tool: &settings_store::display_tools::DisplayTool,
) -> Option<tilo_actions::ActionBinding<()>> {
    tilo_actions::ActionBinding::unit_for(&display_tool_resource(tool), "forget").ok()
}

fn display_control_resource(action: ActionSpec) -> Resource {
    Resource::new(
        pane_resource_id(Category::Displays),
        "settings.section",
        "Displays",
    )
    .with_action(action)
}

pub fn display_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let action = match action_id {
        "auto_brightness" => local_bool_spec(
            "auto_brightness",
            "Set automatic brightness",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetAutoBrightness { enabled },
                )
            },
        ),
        "night_light" => local_bool_spec(
            "night_light",
            "Set Night Light",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLight { enabled },
                )
            },
        ),
        "night_light_schedule" => local_bool_spec(
            "night_light_schedule",
            "Set Night Light schedule",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLightSchedule { enabled },
                )
            },
        ),
        _ => return None,
    };
    bool_binding_for(&display_control_resource(action), action_id)
}

pub fn display_choice_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let action = match action_id {
        "night_light_temperature" => local_choice_spec(
            action_id,
            "Set Night Light warmth",
            DISPLAY_OPERATION_TYPE,
            vec!["Subtle · 4500 K", "Warm · 3700 K", "Very warm · 2800 K"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            &["device-local", "display", "hardware"],
            |label| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLightTemperature { label },
                )
            },
        ),
        _ => return None,
    };
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn display_number_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<f64>> {
    let action = match action_id {
        "brightness" => local_number_spec(
            "brightness",
            "Set brightness",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |value| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetBrightness { value },
                )
            },
        ),
        _ => return None,
    };
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

pub fn display_scale_binding() -> Option<tilo_actions::ActionBinding<String>> {
    let action = local_choice_spec(
        "scale",
        "Set display scale",
        DISPLAY_OPERATION_TYPE,
        vec!["100%", "125%", "150%", "175%", "200%"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetScale { label },
            )
        },
    );
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn display_unit_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<()>> {
    let resource = displays_section(&AutoCommit::new(), &SystemPrefs::default());
    tilo_actions::ActionBinding::unit_for(&resource, action_id).ok()
}

/// Binding for the arrangement canvas: a drop becomes the output's `move`
/// action, the same one an agent or script invokes.
pub fn display_output_move_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<(i32, i32)>> {
    let operation_connector = connector.clone();
    let action = local_point_spec(
        "move",
        "Move display",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "live-output"],
        move |x, y| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::MoveOutput {
                    connector: operation_connector.clone(),
                    x,
                    y,
                },
            )
        },
    );
    let resource = display_output_control_resource(&connector, action.clone());
    tilo_actions::ActionBinding::new(&resource, &action, |(x, y): (i32, i32)| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("x".into(), serde_json::Value::from(x));
        arguments.insert("y".into(), serde_json::Value::from(y));
        arguments
    })
    .ok()
}

fn display_output_control_resource(connector: &str, action: ActionSpec) -> Resource {
    Resource::new(
        format!("settings.display-output:{connector}"),
        "settings.display-output",
        connector.to_string(),
    )
    .with_action(action)
}

pub fn display_output_bool_binding(
    action_id: &'static str,
    connector: String,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let operation_connector = connector.clone();
    let action = match action_id {
        "enabled" => local_bool_spec(
            "enabled",
            "Set display enabled",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetEnabled {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        "primary" => local_bool_spec(
            "primary",
            "Set primary display",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |primary| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetPrimary {
                        connector: operation_connector.clone(),
                        primary,
                    },
                )
            },
        ),
        "hdr" => local_bool_spec(
            "hdr",
            "Set HDR",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetHdr {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        "vrr" => local_bool_spec(
            "vrr",
            "Set variable refresh rate",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetVrr {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        _ => return None,
    };
    bool_binding_for(
        &display_output_control_resource(&connector, action),
        action_id,
    )
}

fn output_brightness_spec(connector: String) -> ActionSpec {
    local_number_spec(
        "brightness",
        "Set monitor brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        move |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetOutputBrightness {
                    connector: connector.clone(),
                    value,
                },
            )
        },
    )
}

fn sdr_brightness_spec(connector: String) -> ActionSpec {
    local_number_spec(
        "sdr-brightness",
        "Set SDR content brightness in HDR",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "live-output"],
        move |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetSdrBrightness {
                    connector: connector.clone(),
                    value,
                },
            )
        },
    )
}

/// The SDR content brightness slider of an HDR-capable monitor's card.
pub fn display_output_sdr_brightness_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<f64>> {
    let resource =
        display_output_control_resource(&connector, sdr_brightness_spec(connector.clone()));
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

/// The brightness slider of one monitor's card.
pub fn display_output_brightness_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<f64>> {
    let resource =
        display_output_control_resource(&connector, output_brightness_spec(connector.clone()));
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

pub fn display_output_choice_binding(
    action_id: &'static str,
    connector: String,
    choices: Vec<String>,
) -> Option<tilo_actions::ActionBinding<String>> {
    if choices.is_empty() || choices.len() > MAX_DYNAMIC_CHOICES {
        return None;
    }
    let operation_connector = connector.clone();
    let label = match action_id {
        "scale" => "Set output scale",
        "position" => "Set output position",
        "mirror" => "Set output mirroring",
        "mode" => "Set resolution and refresh rate",
        "transform" => "Set display orientation",
        "color-profile" => "Set colour profile",
        _ => return None,
    };
    let action = local_choice_spec(
        action_id,
        label,
        DISPLAY_OPERATION_TYPE,
        choices,
        &["device-local", "display", "live-output"],
        move |label| {
            let operation = match action_id {
                "scale" => DisplayOperation::SetOutputScale {
                    connector: operation_connector.clone(),
                    label,
                },
                "position" => DisplayOperation::SetPosition {
                    connector: operation_connector.clone(),
                    label,
                },
                "mirror" => DisplayOperation::SetMirror {
                    connector: operation_connector.clone(),
                    label,
                },
                "mode" => DisplayOperation::SetModeLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                "transform" => DisplayOperation::SetTransformLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                "color-profile" => DisplayOperation::SetColorProfileLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                _ => unreachable!("action id is validated before constructing the closure"),
            };
            encode_local_operation(DISPLAY_OPERATION_TYPE, &operation)
        },
    );
    let resource = display_output_control_resource(&connector, action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

fn network_section(_doc: &AutoCommit) -> Resource {
    let mut sec = Resource::new("settings.section:network", "settings.section", "Network");
    sec = sec.with_action(local_bool_spec(
        "wifi",
        "Set Wi-Fi power",
        NETWORK_OPERATION_TYPE,
        &["device-local", "network", "hardware"],
        |enabled| {
            encode_local_operation(
                NETWORK_OPERATION_TYPE,
                &NetworkOperation::SetWifiEnabled { enabled },
            )
        },
    ));
    // These controls address adapters and radios present on *this* machine.
    // They are local effects with a status lifecycle, not shared settings.
    for (id, label, operation) in [
        ("refresh", "Refresh network list", NetworkOperation::Refresh),
        (
            "run-diagnostics",
            "Run network diagnostics",
            NetworkOperation::RunDiagnostics,
        ),
        (
            "enable-wired",
            "Enable wired networking",
            NetworkOperation::SetWiredEnabled { enabled: true },
        ),
        (
            "disable-wired",
            "Disable wired networking",
            NetworkOperation::SetWiredEnabled { enabled: false },
        ),
        (
            "enable-airplane-mode",
            "Enable airplane mode",
            NetworkOperation::SetAirplaneMode { enabled: true },
        ),
        (
            "disable-airplane-mode",
            "Disable airplane mode",
            NetworkOperation::SetAirplaneMode { enabled: false },
        ),
        (
            "enable-hotspot",
            "Enable hotspot",
            NetworkOperation::SetHotspot { enabled: true },
        ),
        (
            "disable-hotspot",
            "Disable hotspot",
            NetworkOperation::SetHotspot { enabled: false },
        ),
        (
            "enable-mobile-broadband",
            "Enable mobile broadband",
            NetworkOperation::SetMobileBroadband { enabled: true },
        ),
        (
            "disable-mobile-broadband",
            "Disable mobile broadband",
            NetworkOperation::SetMobileBroadband { enabled: false },
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            NETWORK_OPERATION_TYPE,
            &operation,
            &["device-local", "network"],
        ) {
            sec = sec.with_action(action);
        }
    }
    sec
}

fn bool_binding_for(
    resource: &Resource,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

pub fn network_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    bool_binding_for(&network_section(&AutoCommit::new()), action_id)
}

// Inherent resource publication (was ResourceProvider)
impl crate::SettingsApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        let category = category_for_resource_id(id)?;
        let prefs = SystemPrefs::load();
        find_resource(section_resource(category, doc, &prefs), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ExecutionLocus;

    fn find<'a>(sections: &'a [Resource], id: &str) -> &'a Resource {
        sections[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("section {id} present"))
    }

    /// Ctrl+K tree, stable pane addresses, lookups, bounded inventories and refused stale targets.
    #[test]
    fn settings_tree_addresses_are_stable_shallow_and_bounded() {
        // Case `pane_addresses_are_stable_and_unique`.
        {
            let mut addresses = std::collections::HashSet::new();
            for category in Category::ALL.iter().copied() {
                let address = open_pane_address(category);
                assert_eq!(
                    address.resource,
                    format!("settings.section:{}", category.slug())
                );
                assert_eq!(address.action, OPEN_PANE_ACTION);
                assert!(
                    addresses.insert(address),
                    "duplicate address for {category:?}"
                );
            }
        }
    }

    /// Account removal, storage reclamation and update operations need host confirmation.
    #[test]
    fn destructive_resources_require_host_confirmation() {}

    /// Network, Bluetooth, display tokens and display scale are bounded device-local effects.
    #[test]
    fn hardware_controls_are_bounded_typed_local_effects() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `hardware_network_controls_are_bounded_local_effects`.
        {
            let network = find(&resources, &pane_resource_id(Category::Network));
            for id in [
                "refresh",
                "run-diagnostics",
                "enable-wired",
                "disable-wired",
                "enable-airplane-mode",
                "disable-airplane-mode",
                "enable-hotspot",
                "disable-hotspot",
                "enable-mobile-broadband",
                "disable-mobile-broadband",
            ] {
                let action = network
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .unwrap_or_else(|| panic!("missing network operation: {id}"));
                assert_eq!(action.kind, OperationKind::Effect);
                assert_eq!(action.locus, ExecutionLocus::ClientLocal);
                assert_eq!(
                    action.operation_type.as_deref(),
                    Some(NETWORK_OPERATION_TYPE)
                );
                assert!(action.contract.tags.iter().any(|tag| tag == "device-local"));
            }
        }

        // Case `display_operation_tokens_are_typed_and_stable`.
        {
            let operation = DisplayOperation::SetMode {
                connector: "card0/HDMI-A-1".into(),
                mode: settings_store::hardware::OutputMode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 143_850,
                },
            };
            let payload = serde_json::to_vec(&operation).expect("display operation serializes");
            assert_eq!(
                local_operation_token(DISPLAY_OPERATION_TYPE, &payload),
                Ok(local_operation_token_for(
                    DISPLAY_OPERATION_TYPE,
                    &operation
                ))
            );
            assert!(
                local_operation_token(DISPLAY_OPERATION_TYPE, br#"{\"set_mode\":{}}"#).is_err()
            );
        }

        // Case `display_scale_resource_uses_the_dropdowns_durable_label`.
        // Display scale is a device-local typed effect rather than a replicated
        // document write. The operation still carries the exact picker label so
        // agents and the visible control share one validated contract.
        {
            let displays = find(&resources, &pane_resource_id(Category::Displays));
            let scale = displays
                .actions
                .iter()
                .find(|action| action.id == "scale")
                .expect("Displays exports a canonical scale action");
            let args = serde_json::Map::from_iter([(
                "choice".to_owned(),
                serde_json::Value::String("150%".to_owned()),
            )]);
            let encoded = scale
                .build_json(&args)
                .expect("the visible display scale is accepted");
            assert_eq!(encoded.type_id, DISPLAY_OPERATION_TYPE);
            assert_eq!(
                serde_json::from_slice::<DisplayOperation>(&encoded.payload)
                    .expect("display operation decodes"),
                DisplayOperation::SetScale {
                    label: "150%".into()
                }
            );
        }
    }

    /// Accepted local operations, continuity actions and agent provider configuration.
    #[test]
    fn local_operations_and_continuity_are_typed_effects() {
        // Case `accepted_local_operations_advertise_background_string_results`.
        {
            fn walk(resource: &Resource, out: &mut Vec<ActionSpec>) {
                out.extend(
                    resource
                        .actions
                        .iter()
                        .filter(|action| {
                            matches!(
                                action.operation_type.as_deref(),
                                Some(NETWORK_OPERATION_TYPE | DISPLAY_OPERATION_TYPE)
                            ) && action.kind == OperationKind::Effect
                        })
                        .cloned(),
                );
                for child in &resource.children {
                    walk(child, out);
                }
            }

            let mut actions = Vec::new();
            for root in provide_resources(&AutoCommit::new()) {
                walk(&root, &mut actions);
            }
            assert!(!actions.is_empty());
            for action in actions {
                assert!(matches!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: false }
                ));
                assert!(action.contract.validate_declaration().is_empty());
                assert!(
                    action
                        .contract
                        .result
                        .validate(&tilo_actions::ActionOutput {
                            value: Some(serde_json::Value::String("done".into())),
                            resources: Vec::new(),
                        })
                        .is_ok()
                );
            }
        }
    }
}
