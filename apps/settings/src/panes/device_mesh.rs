//! Devices & Sync — configure seamless continuation and always-on sync.
//!
//! The pane reads and writes the same `RelayConfig` consumed by `tilo-relay`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use tilo_clock::unix_now;
use ui::doc::{read_bool, read_str};
use ui::time::{Duration, Instant, SystemTime};
use ui::widget::{AppModel, ModelPath};

use automerge::transaction::Transactable;
use taffy::{NodeId, TaffyTree};
use tilo_relay_protocol::config::CheckpointPrefixRecovery;
use tilo_relay_protocol::{DeviceRole, RelayConfig, ReplicationIntent};
use tilo_user_identity::RevocationReason;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::continuity::{Capability, ContinuityPrivacy};
use crate::i18n::{t, t_args};
use crate::widgets::{
    StatusTone, caption, card, card_with_action, cards, choice_row_described, rows,
    semantic_status_pill, setting_row_described, value_row,
};
use settings_store::roaming_provenance::{Origin, RoamingProvenance};

const DEFAULT_CONFIG: &str = "/etc/tilo/relay.toml";
const APPROVE_ENROLLMENT_KEY: &str = "device_mesh.approve_enrollment.edit";
const APPROVE_PERSONAL_DEVICE_KEY: &str = "device_mesh.approve_personal_device.edit";
const CONTINUITY_ENABLED_KEY: &str = "device_mesh.continuity.enabled";
const CONTROL_HELPER: &str = "/usr/libexec/tilo-home-peer-control";
const RELAY_ADMIN: &str = "/usr/bin/tilo-relay-admin";

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;
const APPLY_TIMEOUT: Duration = Duration::from_secs(120);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const ENROLLMENT_TIMEOUT: Duration = Duration::from_secs(600);
const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(60);
const ROLE_PERSONAL: &str = "Personal device";
const ROLE_ACTIVE: &str = "Available while active";
const ROLE_SERVER: &str = "Always-on server";
const REPLICATION_ON_DEMAND: &str = "On demand";
const REPLICATION_WORKSPACE: &str = "Workspace state";
const REPLICATION_FULL: &str = "Full workspace copy";
const REVOKE_REASONS: &[&str] = &["Retired", "Lost", "Stolen", "Compromised"];

fn role_shown(role: DeviceRole) -> String {
    t(match role {
        DeviceRole::Interactive => "mesh-role-personal",
        DeviceRole::Hybrid => "mesh-role-active",
        DeviceRole::AlwaysOnServer => "mesh-role-server",
    })
}

fn role_from_label(label: &str) -> Option<DeviceRole> {
    if label == ROLE_PERSONAL || label == role_shown(DeviceRole::Interactive) {
        Some(DeviceRole::Interactive)
    } else if label == ROLE_ACTIVE || label == role_shown(DeviceRole::Hybrid) {
        Some(DeviceRole::Hybrid)
    } else if label == ROLE_SERVER || label == role_shown(DeviceRole::AlwaysOnServer) {
        Some(DeviceRole::AlwaysOnServer)
    } else {
        None
    }
}

#[allow(
    dead_code,
    reason = "tests pin the persisted English replication identifiers"
)]
fn replication_label(intent: &ReplicationIntent) -> &'static str {
    match intent {
        ReplicationIntent::WorkspaceOnly => REPLICATION_WORKSPACE,
        ReplicationIntent::FullWorkspace => REPLICATION_FULL,
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => REPLICATION_ON_DEMAND,
    }
}

fn replication_shown(intent: &ReplicationIntent) -> String {
    t(match intent {
        ReplicationIntent::WorkspaceOnly => "mesh-repl-workspace",
        ReplicationIntent::FullWorkspace => "mesh-repl-full",
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => "mesh-repl-demand",
    })
}

fn replication_from_label(label: &str) -> Option<ReplicationIntent> {
    if label == REPLICATION_ON_DEMAND || label == t("mesh-repl-demand") {
        Some(ReplicationIntent::OnDemand)
    } else if label == REPLICATION_WORKSPACE || label == t("mesh-repl-workspace") {
        Some(ReplicationIntent::WorkspaceOnly)
    } else if label == REPLICATION_FULL || label == t("mesh-repl-full") {
        Some(ReplicationIntent::FullWorkspace)
    } else {
        None
    }
}

fn replication_description(intent: &ReplicationIntent) -> String {
    t(match intent {
        ReplicationIntent::WorkspaceOnly => "mesh-repl-workspace-desc",
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => "mesh-repl-demand-desc",
        ReplicationIntent::FullWorkspace => "mesh-repl-full-desc",
    })
}

fn revoke_shown(id: &str) -> String {
    t(match id {
        "Lost" => "mesh-revoke-lost",
        "Stolen" => "mesh-revoke-stolen",
        "Compromised" => "mesh-revoke-compromised",
        _ => "mesh-revoke-retired",
    })
}

fn mesh_status_shown(status: &str) -> String {
    if status == "Saved" {
        t("mesh-status-shown-saved")
    } else if status == "Saving changes…" {
        t("mesh-status-shown-saving")
    } else if status == "Saving latest changes…" {
        t("mesh-status-shown-saving-latest")
    } else if status == "Home server paired" {
        t("mesh-status-shown-home-paired")
    } else if status == "Personal device added" {
        t("mesh-status-shown-device-added")
    } else if let Some(error) = status.strip_prefix("Not saved: ") {
        t_args("mesh-status-shown-not-saved", &[("error", error.into())])
    } else {
        status.to_string()
    }
}

fn shows_advanced_services(role: DeviceRole) -> bool {
    role.can_serve()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RelayRuntimeStatus {
    #[default]
    Offline,
    Online,
    Syncing,
    Ready,
}

impl RelayRuntimeStatus {
    fn read(config: &RelayConfig, now: SystemTime) -> Self {
        let metrics_path = config
            .state_root
            .join(tilo_relay_protocol::metrics::METRICS_FILE);
        let fresh = std::fs::metadata(metrics_path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified).unwrap_or(Duration::ZERO) <= STATUS_FRESHNESS
            });
        if !fresh {
            return Self::Offline;
        }
        match tilo_relay_protocol::metrics::load_metrics(&config.state_root) {
            Ok(Some(_))
                if config.device_role == DeviceRole::AlwaysOnServer
                    && config.services.full_content_replication =>
            {
                let ready = std::fs::read(&config.full_replica_status)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .is_some_and(|proof| {
                        let updated = proof
                            .get("updated_at_ms")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        let now_ms = now
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map_or(0, |duration| {
                                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                            });
                        proof.get("state").and_then(serde_json::Value::as_str) == Some("ready")
                            && proof
                                .get("full_content_replication")
                                .and_then(serde_json::Value::as_bool)
                                == Some(true)
                            && now_ms.saturating_sub(updated)
                                <= u64::try_from(STATUS_FRESHNESS.as_millis()).unwrap_or(u64::MAX)
                    });
                if ready { Self::Ready } else { Self::Syncing }
            }
            Ok(Some(_)) => Self::Online,
            Ok(None) | Err(_) => Self::Offline,
        }
    }

    fn label(self) -> String {
        t(match self {
            Self::Offline => "mesh-runtime-offline",
            Self::Online => "mesh-runtime-online",
            Self::Syncing => "mesh-runtime-syncing",
            Self::Ready => "mesh-runtime-ready",
        })
    }
}

fn runtime_status_tone(status: RelayRuntimeStatus) -> StatusTone {
    match status {
        RelayRuntimeStatus::Ready | RelayRuntimeStatus::Online => StatusTone::Success,
        RelayRuntimeStatus::Syncing => StatusTone::Warning,
        RelayRuntimeStatus::Offline => StatusTone::Neutral,
    }
}

/// Editor for the daemon's real device-mesh configuration.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans are independent asynchronous process and widget lifecycle states, not one mutually exclusive state machine"
)]
pub struct DeviceMeshPane {
    config_path: PathBuf,
    config: RelayConfig,
    load_status: String,
    needs_seed: bool,
    commit_context_pending: bool,
    structure_dirty: bool,
    runtime_status: RelayRuntimeStatus,
    discovered_workspaces: Vec<DiscoveredWorkspace>,
    registered_home_peers: usize,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
    home_peer_health: HashMap<String, String>,
    home_peer_last_seen: HashMap<String, u64>,
    personal_devices: Vec<crate::device_fleet::DeviceView>,
    personal_device_error: Option<String>,
    continuity: ContinuityPrivacy,
    continuity_path: Option<PathBuf>,
    continuity_status: Option<String>,
    roaming_provenance_path: Option<PathBuf>,
    roaming_provenance_stamp: Option<SystemTime>,
    roaming_provenance: Option<RoamingProvenance>,
    roaming_provenance_status: Option<String>,
    personal_remove_armed: std::collections::HashSet<[u8; 32]>,
    management_child: Option<bounded_command::AsyncCommand>,
    management_device: Option<String>,
    next_peer_health: usize,
    remove_armed: std::collections::HashSet<String>,
    apply_child: Option<bounded_command::AsyncCommand>,
    apply_queued: bool,
    pairing_child: Option<bounded_command::AsyncCommand>,
    pairing_discovery: bool,
    pairing_candidate: Option<PairCandidate>,
    pairing_target: PairingTarget,
    next_status_refresh: Instant,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    command_state: Option<ui::LocalState<Option<crate::actions::DeviceMeshOperation>>>,
    operation_status: Option<ui::LocalState<crate::actions::LocalOperationStatus>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredWorkspace {
    id: u64,
    name: String,
}

#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
struct PairCandidate {
    device_node_id: String,
    relay_service_node_id: String,
    label: String,
    verification_code: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PairingTarget {
    #[default]
    HomePeer,
    PersonalDevice,
}

fn workspace_key(id: u64) -> String {
    format!("device_mesh.workspace.{id}")
}

fn discover_workspaces(config: &RelayConfig) -> Vec<DiscoveredWorkspace> {
    let path = tilo_paths::config_dir().map_or_else(
        || PathBuf::from("state.json"),
        |dir| dir.join("tilo/state.json"),
    );
    let mut result = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|state| {
            state
                .get("workspaces")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .map(|workspaces| {
            workspaces
                .into_iter()
                .filter_map(|workspace| {
                    let id = workspace
                        .get("room")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|room| tilo_relay_protocol::room_workspace_id(room).ok())
                        .or_else(|| workspace.get("id").and_then(serde_json::Value::as_u64))?;
                    let name = workspace
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Workspace")
                        .to_owned();
                    Some(DiscoveredWorkspace { id, name })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for binding in &config.workspaces {
        if !result.iter().any(|workspace| workspace.id == binding.id) {
            result.push(DiscoveredWorkspace {
                id: binding.id,
                name: format!("Workspace {}", binding.id),
            });
        }
    }
    result.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    result
}

pub(crate) fn resource_workspaces() -> Vec<(u64, String)> {
    discover_workspaces(&RelayConfig::default())
        .into_iter()
        .map(|workspace| (workspace.id, workspace.name))
        .collect()
}

fn registered_home_peer_count() -> usize {
    registered_home_peers().len()
}

fn registered_home_peers() -> Vec<tilo_relay_protocol::HomePeerRegistration> {
    std::fs::read(identity_root().join("home-peers.json"))
        .ok()
        .filter(|bytes| bytes.len() <= 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn peer_key(action: &str, device: &str) -> String {
    format!("device_mesh.home_peer.{action}.{device}.edit")
}

fn personal_device_key(action: &str, device: &[u8; 32]) -> String {
    format!(
        "device_mesh.personal_device.{action}.{}.edit",
        hex::encode(device)
    )
}

fn continuity_key(capability: Capability) -> String {
    format!("device_mesh.continuity.{}", capability.key())
}

fn origin_summary(origin: &Origin) -> String {
    format!("{} · revision {}", origin.device_label, origin.counter)
}

fn resolution_summary(winner: &Origin, alternatives: &[Origin]) -> String {
    if alternatives.is_empty() {
        return t_args("mesh-from", &[("origin", origin_summary(winner).into())]);
    }
    let others = alternatives
        .iter()
        .take(3)
        .map(|origin| origin.device_label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = alternatives.len().saturating_sub(3);
    let suffix = if remainder == 0 {
        String::new()
    } else {
        t_args(
            "mesh-and-more",
            &[("n", i64::try_from(remainder).unwrap_or(0).into())],
        )
    };
    t_args(
        "mesh-resolved",
        &[
            ("winner", origin_summary(winner).into()),
            ("others", others.into()),
            ("suffix", suffix.into()),
        ],
    )
}

fn provenance_summaries(provenance: &RoamingProvenance) -> Vec<(String, String)> {
    provenance
        .settings
        .iter()
        .map(|setting| {
            (
                setting.key.clone(),
                resolution_summary(&setting.winner, &setting.alternatives),
            )
        })
        .chain(provenance.wifi.iter().map(|wifi| {
            (
                t_args("mesh-wifi-row", &[("ssid", wifi.ssid.as_str().into())]),
                t_args(
                    "mesh-wifi-sealed",
                    &[
                        ("security", wifi.security.as_str().into()),
                        (
                            "detail",
                            resolution_summary(&wifi.winner, &wifi.alternatives).into(),
                        ),
                    ],
                ),
            )
        }))
        .collect()
}

fn load_roaming_provenance(
    path: Option<&std::path::Path>,
) -> (
    Option<RoamingProvenance>,
    Option<String>,
    Option<SystemTime>,
) {
    let Some(path) = path else {
        return (None, None, None);
    };
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if stamp.is_none() {
        return (None, None, None);
    }
    match RoamingProvenance::load_at(path) {
        Ok(provenance) => (Some(provenance), None, stamp),
        Err(error) => (
            None,
            Some(t_args(
                "mesh-provenance-err",
                &[("error", error.to_string().into())],
            )),
            stamp,
        ),
    }
}

fn revocation_reason(label: &str) -> RevocationReason {
    if label == "Lost" || label == t("mesh-revoke-lost") {
        RevocationReason::Lost
    } else if label == "Stolen" || label == t("mesh-revoke-stolen") {
        RevocationReason::Stolen
    } else if label == "Compromised" || label == t("mesh-revoke-compromised") {
        RevocationReason::Compromised
    } else {
        RevocationReason::Retired
    }
}

fn home_peer_health_path() -> PathBuf {
    identity_root().join("home-peer-health.json")
}

fn load_home_peer_last_seen() -> HashMap<String, u64> {
    std::fs::read(home_peer_health_path())
        .ok()
        .filter(|bytes| bytes.len() <= 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_home_peer_last_seen(last_seen: &HashMap<String, u64>) {
    if let Ok(bytes) = serde_json::to_vec_pretty(last_seen) {
        let _ =
            tilo_files::native::atomic::write_with_mode(&home_peer_health_path(), &bytes, 0o600);
    }
}

fn last_seen_label(last_seen_ms: u64) -> String {
    let now_ms = ui::time::wall()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    let minutes = now_ms.saturating_sub(last_seen_ms) / 60_000;
    if minutes == 0 {
        t("mesh-last-seen-now")
    } else if minutes < 60 {
        t_args(
            "mesh-last-seen-min",
            &[("n", i64::try_from(minutes).unwrap_or(0).into())],
        )
    } else {
        t_args(
            "mesh-last-seen-h",
            &[("n", i64::try_from(minutes / 60).unwrap_or(0).into())],
        )
    }
}

fn peer_health_badge(health: &str) -> String {
    if health.starts_with("Online") && health.contains("Syncing") {
        t("mesh-badge-syncing")
    } else if health.starts_with("Online") {
        t("mesh-badge-online")
    } else if health == "Checking…" || health == "Connecting…" || health == t("mesh-checking") {
        t("mesh-badge-checking")
    } else if health == "Removed" {
        t("mesh-badge-removed")
    } else {
        t("mesh-badge-offline")
    }
}

pub(crate) fn identity_root() -> PathBuf {
    std::env::var_os("TILO_IDENTITY_ROOT").map_or_else(
        || {
            if PathBuf::from("/var/lib/tilo/device").is_dir() {
                PathBuf::from("/var/lib/tilo")
            } else {
                tilo_paths::data_dir()
                    .unwrap_or_else(|| PathBuf::from(".local/share"))
                    .join("tilo")
            }
        },
        PathBuf::from,
    )
}

fn command_error(stderr: &[u8], fallback: &str) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        fallback.to_owned()
    } else {
        detail
            .chars()
            .filter(|character| !character.is_control() || *character == ' ')
            .take(512)
            .collect()
    }
}

impl DeviceMeshPane {
    /// Load the system relay configuration. Missing configuration is a normal
    /// first-run state and starts from the daemon's defaults.
    pub fn new() -> Self {
        Self::from_path(PathBuf::from(DEFAULT_CONFIG))
    }

    fn from_path(config_path: PathBuf) -> Self {
        let (config, load_status) = match RelayConfig::load(&config_path) {
            Ok(config) => (config, "Configuration loaded".to_string()),
            Err(tilo_relay_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                // Settings is opened on interactive Tilo devices far more
                // often than on a newly provisioned headless relay. Do not
                // visually opt an unconfigured desktop into server behavior.
                let config = RelayConfig {
                    device_role: DeviceRole::Interactive,
                    services: tilo_relay_protocol::ServiceCapabilities::default(),
                    ..RelayConfig::default()
                };
                (config, "Not configured on this device".to_string())
            }
            Err(error) => (
                RelayConfig::default(),
                format!("Configuration could not be read: {error}"),
            ),
        };
        let runtime_status = RelayRuntimeStatus::read(&config, ui::time::wall());
        let discovered_workspaces = discover_workspaces(&config);
        let home_peers = registered_home_peers();
        let (personal_devices, personal_device_error) = match crate::device_fleet::list() {
            Ok(devices) => (devices, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let continuity_path = crate::continuity::settings_path();
        let continuity = continuity_path
            .as_deref()
            .map_or_else(ContinuityPrivacy::default, ContinuityPrivacy::load_at);
        let roaming_provenance_path = settings_store::roaming_provenance::path();
        let (roaming_provenance, roaming_provenance_status, roaming_provenance_stamp) =
            load_roaming_provenance(roaming_provenance_path.as_deref());
        Self {
            config_path,
            config,
            load_status,
            needs_seed: true,
            commit_context_pending: false,
            structure_dirty: false,
            runtime_status,
            discovered_workspaces,
            registered_home_peers: home_peers.len(),
            home_peers,
            home_peer_health: HashMap::new(),
            home_peer_last_seen: load_home_peer_last_seen(),
            personal_devices,
            personal_device_error,
            continuity,
            continuity_path,
            continuity_status: None,
            roaming_provenance_path,
            roaming_provenance_stamp,
            roaming_provenance,
            roaming_provenance_status,
            personal_remove_armed: std::collections::HashSet::new(),
            management_child: None,
            management_device: None,
            next_peer_health: 0,
            remove_armed: std::collections::HashSet::new(),
            apply_child: None,
            apply_queued: false,
            pairing_child: None,
            pairing_discovery: false,
            pairing_candidate: None,
            pairing_target: PairingTarget::HomePeer,
            next_status_refresh: ui::time::now() + STATUS_REFRESH_INTERVAL,
            composed: None,
            rect: Rect::ZERO,
            command_state: None,
            operation_status: None,
        }
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        state: &ui::LocalState<Option<crate::actions::DeviceMeshOperation>>,
    ) -> Self {
        self.command_state = Some(state.clone());
        self
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = Some(status.clone());
        self
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        let mut pane = Self::from_path(path.to_path_buf());
        let continuity_path = path.with_file_name("settings.json");
        pane.continuity = ContinuityPrivacy::load_at(&continuity_path);
        pane.continuity_path = Some(continuity_path);
        pane.roaming_provenance_path =
            Some(path.with_file_name(settings_store::roaming_provenance::FILE_NAME));
        pane.refresh_roaming_provenance();
        // Swapping the per-user provenance file read above for the fixture's
        // is construction, not a change the host must rebuild for; left set,
        // the flag depended on whether the developer's own file exists.
        pane.structure_dirty = false;
        // The periodic runtime-status poll is wall-clock driven; on a loaded
        // machine its interval can elapse between construction and the first
        // sync and flag a rebuild no test action caused.
        pane.next_status_refresh = ui::time::now() + Duration::from_secs(24 * 60 * 60);
        pane
    }

    fn save_continuity(&mut self) {
        self.continuity_status = match self.continuity.save_to(self.continuity_path.as_deref()) {
            Ok(()) => None,
            Err(error) => Some(format!("Continuity choices were not saved: {error}")),
        };
    }

    fn common_replication(&self) -> ReplicationIntent {
        self.config
            .workspaces
            .first()
            .map_or(ReplicationIntent::OnDemand, |workspace| {
                workspace.replication.clone()
            })
    }

    fn finish_command(
        &self,
        operation: &crate::actions::DeviceMeshOperation,
        result: Result<(), String>,
    ) {
        let Some(status) = &self.operation_status else {
            return;
        };
        let token = crate::actions::local_operation_token_for(
            crate::actions::DEVICE_MESH_OPERATION_TYPE,
            operation,
        );
        status.set(match result {
            Ok(()) => crate::actions::LocalOperationStatus::Succeeded {
                token,
                message: "Device & Sync settings saved on this device".into(),
            },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    fn apply_command(&mut self, operation: &crate::actions::DeviceMeshOperation) {
        let result = match operation {
            crate::actions::DeviceMeshOperation::SetRole { role } => role_from_label(role)
                .ok_or_else(|| "unsupported device role".to_string())
                .map(|role| {
                    self.apply_role_change(role);
                }),
            crate::actions::DeviceMeshOperation::SetReplication { replication } => {
                replication_from_label(replication)
                    .ok_or_else(|| "unsupported replication policy".to_string())
                    .map(|intent| {
                        for workspace in &mut self.config.workspaces {
                            workspace.replication = intent.clone();
                        }
                        self.config.services.full_content_replication =
                            intent == ReplicationIntent::FullWorkspace;
                    })
            }
            crate::actions::DeviceMeshOperation::SetService { service, enabled } => {
                match service.as_str() {
                    "inbound_relay" => self.config.services.inbound_relay = *enabled,
                    "remote_control_relay" => {
                        self.config.services.remote_control_relay = *enabled;
                    }
                    "app_serving" => self.config.services.app_serving = *enabled,
                    "compute" => self.config.services.compute = *enabled,
                    "continuity.enabled" => {
                        self.continuity.enabled = *enabled;
                        self.continuity.record(None, *enabled, unix_now());
                        self.save_continuity();
                    }
                    service if service.starts_with("continuity.") => {
                        let key = service.strip_prefix("continuity.").unwrap_or_default();
                        let Some(capability) = Capability::ALL
                            .into_iter()
                            .find(|capability| capability.key() == key)
                        else {
                            self.finish_command(
                                operation,
                                Err("unsupported Continuity capability".into()),
                            );
                            return;
                        };
                        self.continuity.set_allowed(capability, *enabled);
                        self.continuity
                            .record(Some(capability), *enabled, unix_now());
                        self.save_continuity();
                    }
                    _ => {
                        self.finish_command(operation, Err("unsupported device service".into()));
                        return;
                    }
                }
                Ok(())
            }
            crate::actions::DeviceMeshOperation::SetWorkspace { id, enabled } => {
                if self
                    .discovered_workspaces
                    .iter()
                    .any(|workspace| workspace.id == *id)
                {
                    if *enabled
                        && !self
                            .config
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.id == *id)
                    {
                        self.config
                            .workspaces
                            .push(tilo_relay_protocol::WorkspaceBinding {
                                id: *id,
                                pin: self.config.device_role.can_serve(),
                                replication: ReplicationIntent::OnDemand,
                                admitted_users: Vec::new(),
                                // Workspace discovery proves only that the room exists. It
                                // cannot confer the native shell governor's retained authority.
                                shell_authority: None,
                                checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
                            });
                    } else if !*enabled {
                        self.config.remove_workspace(*id);
                    }
                    Ok(())
                } else {
                    Err("workspace is no longer available on this device".into())
                }
            }
        };
        if result.is_ok() {
            self.persist();
            self.structure_dirty = true;
        }
        self.finish_command(operation, result);
    }

    fn refresh_runtime_status(&mut self) {
        let status = RelayRuntimeStatus::read(&self.config, ui::time::wall());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.structure_dirty = true;
        }
        self.refresh_roaming_provenance();
        self.next_status_refresh = ui::time::now() + STATUS_REFRESH_INTERVAL;
    }

    fn refresh_roaming_provenance(&mut self) {
        let stamp = self
            .roaming_provenance_path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok());
        if stamp == self.roaming_provenance_stamp {
            return;
        }
        let (projection, status, stamp) =
            load_roaming_provenance(self.roaming_provenance_path.as_deref());
        self.roaming_provenance = projection;
        self.roaming_provenance_status = status;
        self.roaming_provenance_stamp = stamp;
        self.structure_dirty = true;
    }

    fn apply_role_change(&mut self, role: DeviceRole) -> bool {
        if role == self.config.device_role {
            return false;
        }
        if shows_advanced_services(role) != shows_advanced_services(self.config.device_role) {
            self.structure_dirty = true;
        }
        self.config.device_role = role;
        self.config.services.workspace_replication = role.can_serve();
        self.config.services.inbound_relay = role.can_serve();
        for workspace in &mut self.config.workspaces {
            workspace.pin = role.can_serve();
        }
        if role == DeviceRole::AlwaysOnServer {
            if self.config.workspaces.is_empty() {
                self.config.workspaces = self
                    .discovered_workspaces
                    .iter()
                    .map(|workspace| tilo_relay_protocol::WorkspaceBinding {
                        id: workspace.id,
                        pin: true,
                        replication: ReplicationIntent::FullWorkspace,
                        admitted_users: Vec::new(),
                        shell_authority: None,
                        checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
                    })
                    .collect();
            }
            self.config.services.full_content_replication = true;
            for workspace in &mut self.config.workspaces {
                workspace.replication = ReplicationIntent::FullWorkspace;
            }
        } else {
            self.config.services.full_content_replication = false;
        }
        self.next_status_refresh = ui::time::now();
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the pane composition keeps the four device-mesh policy cards and their explanatory security text in one auditable hierarchy"
    )]
    fn build_composition(&self) -> Box<dyn Widget> {
        let role_shown_value = role_shown(self.config.device_role);
        let role_state = ui::LocalState::new(role_shown_value.clone());
        let role = Dropdown::from_state(
            [
                DeviceRole::Interactive,
                DeviceRole::Hybrid,
                DeviceRole::AlwaysOnServer,
            ]
            .into_iter()
            .map(role_shown)
            .collect(),
            &role_state,
        )
        .with_id("device_mesh.role")
        .with_placeholder(&role_shown_value);
        let role = match crate::actions::device_mesh_text_binding("role") {
            Some(binding) => role.on_action(binding),
            None => role.with_enabled(false),
        };

        let activity = t("mesh-activity");
        let sync_auto = t("mesh-sync-auto");
        let files = t("mesh-files");
        let download_opened = t("mesh-download-opened");
        let mut continuation_rows = vec![
            value_row(&activity, &sync_auto),
            caption(&t("mesh-activity-caption")),
        ];
        if self.config.workspaces.is_empty() {
            continuation_rows.push(value_row(&files, &download_opened));
        } else {
            let repl_shown = replication_shown(&self.common_replication());
            let replication_state = ui::LocalState::new(repl_shown.clone());
            let replication = Dropdown::from_state(
                [
                    ReplicationIntent::OnDemand,
                    ReplicationIntent::WorkspaceOnly,
                    ReplicationIntent::FullWorkspace,
                ]
                .into_iter()
                .map(|intent| replication_shown(&intent))
                .collect(),
                &replication_state,
            )
            .with_id("device_mesh.replication")
            .with_placeholder(&repl_shown);
            let replication = match crate::actions::device_mesh_text_binding("replication") {
                Some(binding) => replication.on_action(binding),
                None => replication.with_enabled(false),
            };
            let repl_desc = replication_description(&self.common_replication());
            continuation_rows.push(choice_row_described(
                &files,
                &repl_desc,
                Box::new(replication),
            ));
        }
        if self.load_status.starts_with("Configuration could not")
            || self.load_status.starts_with("Not saved")
            || self.load_status.starts_with("Saving")
            || self.load_status == "Saved"
            || self.load_status.starts_with("Waiting")
            || self.load_status.starts_with("Could not start pairing")
        {
            continuation_rows.push(caption(&mesh_status_shown(&self.load_status)));
        }

        let this_device = t("mesh-this-device");
        let this_device_desc = t("mesh-this-device-desc");
        let mut availability_rows = vec![choice_row_described(
            &this_device,
            &this_device_desc,
            Box::new(role),
        )];
        if shows_advanced_services(self.config.device_role) {
            let home_peer = t("mesh-home-peer");
            let home_desc = t(match self.runtime_status {
                RelayRuntimeStatus::Ready => "mesh-home-ready",
                RelayRuntimeStatus::Online => "mesh-home-online",
                RelayRuntimeStatus::Syncing => "mesh-home-syncing",
                RelayRuntimeStatus::Offline => "mesh-home-offline",
            });
            let runtime = self.runtime_status.label();
            availability_rows.push(setting_row_described(
                &home_peer,
                &home_desc,
                semantic_status_pill(&runtime, runtime_status_tone(self.runtime_status)),
            ));
        }
        let workspaces = t("mesh-workspaces");
        availability_rows.push(value_row(
            &workspaces,
            &self.config.workspaces.len().to_string(),
        ));
        availability_rows.push(caption(&t(match self.config.device_role {
            DeviceRole::Interactive => "mesh-role-hint-personal",
            DeviceRole::Hybrid => "mesh-role-hint-hybrid",
            DeviceRole::AlwaysOnServer if self.config.workspaces.is_empty() => {
                "mesh-role-hint-server-empty"
            }
            DeviceRole::AlwaysOnServer
                if self.common_replication() == ReplicationIntent::FullWorkspace =>
            {
                "mesh-role-hint-server-full"
            }
            DeviceRole::AlwaysOnServer => "mesh-role-hint-server",
        })));

        let workspace_rows = if self.discovered_workspaces.is_empty() {
            rows(vec![caption(&t("mesh-workspaces-empty"))])
        } else {
            rows(
                self.discovered_workspaces
                    .iter()
                    .map(|workspace| {
                        let key = workspace_key(workspace.id);
                        let selected = self
                            .config
                            .workspaces
                            .iter()
                            .any(|binding| binding.id == workspace.id);
                        setting_row_described(
                            &workspace.name,
                            &t("mesh-keep-workspace"),
                            Box::new(
                                crate::actions::device_mesh_workspace_binding(workspace.id)
                                    .map_or_else(
                                        || {
                                            Toggle::from_state(&ui::LocalState::new(selected))
                                                .with_id(&key)
                                                .with_enabled(false)
                                        },
                                        |binding| {
                                            Toggle::from_state(&ui::LocalState::new(selected))
                                                .with_id(&key)
                                                .on_action(binding)
                                        },
                                    ),
                            ),
                        )
                    })
                    .collect(),
            )
        };
        let pairing = self.pairing_child.is_some();
        let pair_label = if pairing && self.pairing_target == PairingTarget::HomePeer {
            t("mesh-pairing")
        } else if let Some(candidate) = self
            .pairing_candidate
            .as_ref()
            .filter(|_| self.pairing_target == PairingTarget::HomePeer)
        {
            t_args(
                "mesh-approve",
                &[
                    ("label", candidate.label.as_str().into()),
                    ("code", candidate.verification_code.as_str().into()),
                ],
            )
        } else {
            t("mesh-pair-home")
        };
        let approve = Box::new(
            Button::new(
                "device_mesh.approve_enrollment",
                &pair_label,
                Some(APPROVE_ENROLLMENT_KEY),
            )
            .content_sized()
            .with_enabled(!pairing),
        );
        let personal_label = if pairing && self.pairing_target == PairingTarget::PersonalDevice {
            t("mesh-pairing")
        } else if self.pairing_target == PairingTarget::PersonalDevice {
            self.pairing_candidate.as_ref().map_or_else(
                || t("mesh-add-device"),
                |candidate| {
                    t_args(
                        "mesh-approve",
                        &[
                            ("label", candidate.label.as_str().into()),
                            ("code", candidate.verification_code.as_str().into()),
                        ],
                    )
                },
            )
        } else {
            t("mesh-add-device")
        };
        let approve_personal = Box::new(
            Button::new(
                "device_mesh.approve_personal_device",
                &personal_label,
                Some(APPROVE_PERSONAL_DEVICE_KEY),
            )
            .content_sized()
            .with_enabled(!pairing),
        );
        let mut personal_rows = vec![caption(&t("mesh-personal-caption"))];
        if self.pairing_target == PairingTarget::PersonalDevice
            && (pairing || self.pairing_candidate.is_some() || !self.load_status.is_empty())
        {
            personal_rows.push(caption(&mesh_status_shown(&self.load_status)));
        }
        let mut enrolled_device_rows = Vec::new();
        for device in &self.personal_devices {
            let armed = self.personal_remove_armed.contains(&device.key);
            let revoke_key = personal_device_key("revoke", &device.key);
            let reason_key = personal_device_key("reason", &device.key);
            let status = t(if device.revoked {
                "mesh-status-revoked"
            } else if device.current {
                "mesh-status-this-device"
            } else if device.can_host {
                "mesh-status-home-server"
            } else {
                "mesh-status-trusted"
            });
            let fingerprint = t_args(
                "mesh-fingerprint",
                &[("id", device.fingerprint.as_str().into())],
            );
            enrolled_device_rows.push(setting_row_described(
                &device.label,
                &fingerprint,
                if armed {
                    Box::new(
                        Button::new(&revoke_key, &t("mesh-revoke-access"), Some(&revoke_key))
                            .content_sized()
                            .with_enabled(!device.current && !device.revoked),
                    )
                } else if device.current || device.revoked || device.can_host {
                    semantic_status_pill(
                        &status,
                        if device.current {
                            StatusTone::Success
                        } else {
                            StatusTone::Neutral
                        },
                    )
                } else {
                    Box::new(
                        Button::new(&revoke_key, &t("mesh-decommission"), Some(&revoke_key))
                            .content_sized(),
                    )
                },
            ));
            if armed {
                let cancel_key = personal_device_key("cancel", &device.key);
                let why = t("mesh-why-ending");
                let why_desc = t("mesh-why-ending-desc");
                let retired = t("mesh-revoke-retired");
                enrolled_device_rows.push(choice_row_described(
                    &why,
                    &why_desc,
                    Box::new(
                        Dropdown::new(
                            REVOKE_REASONS.iter().map(|id| revoke_shown(id)).collect(),
                            &reason_key,
                        )
                        .with_id(&reason_key)
                        .with_placeholder(&retired),
                    ),
                ));
                let ends = t("mesh-access-ends");
                let ends_desc = t("mesh-access-ends-desc");
                enrolled_device_rows.push(setting_row_described(
                    &ends,
                    &ends_desc,
                    Box::new(
                        Button::new(&cancel_key, &t("mesh-cancel"), Some(&cancel_key))
                            .content_sized(),
                    ),
                ));
            }
        }
        if let Some(error) = &self.personal_device_error {
            enrolled_device_rows.push(caption(&t_args(
                "mesh-device-list-err",
                &[("error", error.as_str().into())],
            )));
        } else if enrolled_device_rows.is_empty() {
            enrolled_device_rows.push(caption(&t("mesh-device-list-empty")));
        }

        let current_device = self
            .personal_devices
            .iter()
            .find(|device| device.current && !device.revoked);
        let current_target = self
            .personal_devices
            .iter()
            .find(|device| !device.current && !device.revoked);
        let no_target = t("mesh-no-target");
        let target_name = current_target.map_or(no_target.as_str(), |device| &device.label);
        let target_detail = current_target.map_or_else(
            || t("mesh-add-target"),
            |device| {
                t_args(
                    "mesh-certified",
                    &[("id", device.fingerprint.as_str().into())],
                )
            },
        );
        let source_detail = current_device.map_or_else(
            || t("mesh-source-missing"),
            |device| {
                t_args(
                    "mesh-source-detail",
                    &[
                        ("label", device.label.as_str().into()),
                        ("id", device.fingerprint.as_str().into()),
                    ],
                )
            },
        );
        let continuity_toggle = crate::actions::device_mesh_bool_binding("continuity-enabled")
            .map_or_else(
                || {
                    Toggle::from_state(&ui::LocalState::new(self.continuity.enabled))
                        .with_enabled(false)
                },
                |binding| {
                    Toggle::from_state(&ui::LocalState::new(self.continuity.enabled))
                        .on_action(binding)
                },
            )
            .with_id(CONTINUITY_ENABLED_KEY);
        let allow = t("mesh-allow-continuity");
        let allow_desc = t("mesh-allow-continuity-desc");
        let mut privacy_rows = vec![setting_row_described(
            &allow,
            &allow_desc,
            Box::new(continuity_toggle),
        )];
        for capability in Capability::ALL {
            let key = continuity_key(capability);
            let desc = capability.description();
            let status = if self.continuity.effective(capability) {
                t_args(
                    "mesh-facet-active",
                    &[
                        ("desc", desc.as_str().into()),
                        ("target", target_name.into()),
                    ],
                )
            } else if self.continuity.allowed(capability) {
                t_args("mesh-facet-prepared", &[("desc", desc.into())])
            } else {
                t_args("mesh-facet-off", &[("desc", desc.into())])
            };
            let action_id = format!("continuity-{}", capability.key().replace('_', "-"));
            let toggle = crate::actions::device_mesh_bool_binding(&action_id)
                .map_or_else(
                    || {
                        Toggle::from_state(&ui::LocalState::new(
                            self.continuity.allowed(capability),
                        ))
                        .with_enabled(false)
                    },
                    |binding| {
                        Toggle::from_state(&ui::LocalState::new(
                            self.continuity.allowed(capability),
                        ))
                        .on_action(binding)
                    },
                )
                .with_id(&key);
            let cap_label = capability.label();
            privacy_rows.push(setting_row_described(&cap_label, &status, Box::new(toggle)));
        }
        privacy_rows.push(caption(&t("mesh-privacy-stay")));
        if let Some(status) = &self.continuity_status {
            privacy_rows.push(caption(status));
        }

        let current_target_label = t("mesh-current-target");
        let source_label = t("mesh-source");
        let mut provenance_rows = vec![
            value_row(&current_target_label, target_name),
            caption(&target_detail),
            value_row(&source_label, &source_detail),
        ];
        if current_target.is_none() {
            provenance_rows.push(caption(&t("mesh-no-active-until")));
        }

        let audit_rows = if self.continuity.audit.is_empty() {
            vec![caption(&t("mesh-audit-empty"))]
        } else {
            self.continuity
                .audit
                .iter()
                .rev()
                .take(6)
                .map(|entry| {
                    let change = t("mesh-audit-change");
                    value_row(&change, &entry.summary())
                })
                .chain(std::iter::once(caption(&t("mesh-audit-caption"))))
                .collect()
        };
        let mut roaming_rows = self.roaming_provenance.as_ref().map_or_else(
            || vec![caption(&t("mesh-roaming-empty"))],
            |provenance| {
                let summaries = provenance_summaries(provenance);
                if summaries.is_empty() {
                    vec![caption(&t("mesh-roaming-none"))]
                } else {
                    summaries
                        .into_iter()
                        .map(|(name, detail)| value_row(&name, &detail))
                        .collect()
                }
            },
        );
        if let Some(status) = &self.roaming_provenance_status {
            roaming_rows.push(caption(status));
        }
        roaming_rows.push(caption(&t("mesh-roaming-caption")));
        let mut home_server_rows = vec![caption(&t("mesh-home-caption"))];
        if self.registered_home_peers > 0 {
            let paired = t("mesh-paired-homes");
            home_server_rows.push(value_row(&paired, &self.registered_home_peers.to_string()));
        }
        if pairing || self.load_status.starts_with("Home server") {
            home_server_rows.push(caption(&mesh_status_shown(&self.load_status)));
        }
        let continue_title = t("mesh-continue-title");
        let privacy_title = t("mesh-privacy-title");
        let route_title = t("mesh-route-title");
        let roaming_title = t("mesh-roaming-title");
        let activity_title = t("mesh-activity-title");
        let add_personal = t("mesh-add-personal");
        let trusted_title = t("mesh-trusted-title");
        let availability_title = t("mesh-availability-title");
        let synced_title = t("mesh-synced-title");
        let home_nas = t("mesh-home-nas");
        let mut sections = vec![
            card(&continue_title, rows(continuation_rows)),
            card(&privacy_title, rows(privacy_rows)),
            card(&route_title, rows(provenance_rows)),
            card(&roaming_title, rows(roaming_rows)),
            card(&activity_title, rows(audit_rows)),
            card_with_action(&add_personal, approve_personal, rows(personal_rows)),
            card(&trusted_title, rows(enrolled_device_rows)),
            card(&availability_title, rows(availability_rows)),
            card(&synced_title, workspace_rows),
            card_with_action(&home_nas, approve, rows(home_server_rows)),
        ];
        for peer in &self.home_peers {
            let device = &peer.device_node_id;
            let rename_key = peer_key("name", device);
            let rename_action = peer_key("rename", device);
            let remove_action = peer_key("remove", device);
            let name = TextInput::new(Vec::new())
                .with_id(&rename_key)
                .with_on_change_path(&rename_key)
                .with_placeholder(&peer.label)
                .with_font_size(13.0);
            let rename = Box::new(
                Button::new(&rename_action, &t("mesh-rename"), Some(&rename_action))
                    .content_sized(),
            );
            let armed = self.remove_armed.contains(device);
            let remove_label = if armed {
                t("mesh-revoke-remove")
            } else {
                t("mesh-remove-server")
            };
            let remove = Box::new(
                Button::new(&remove_action, &remove_label, Some(&remove_action)).content_sized(),
            );
            let health = self
                .home_peer_health
                .get(device)
                .cloned()
                .or_else(|| {
                    self.home_peer_last_seen
                        .get(device)
                        .map(|last_seen| last_seen_label(*last_seen))
                })
                .unwrap_or_else(|| t("mesh-checking"));
            let name_label = t("mesh-name");
            let name_this = t("mesh-name-this");
            let apply_name = t("mesh-apply-name");
            let apply_name_desc = t("mesh-apply-name-desc");
            let availability = t("mesh-availability");
            let badge = peer_health_badge(&health);
            let mut peer_rows = vec![
                setting_row_described(&name_label, &name_this, Box::new(name)),
                setting_row_described(&apply_name, &apply_name_desc, rename),
                setting_row_described(
                    &availability,
                    &health,
                    semantic_status_pill(
                        &badge,
                        if health.starts_with("Online") {
                            StatusTone::Success
                        } else if health.contains("Syncing") || health == "Checking…" {
                            StatusTone::Warning
                        } else {
                            StatusTone::Neutral
                        },
                    ),
                ),
            ];
            if armed {
                let cancel_action = peer_key("cancel_remove", device);
                let files_stay = t("mesh-files-stay");
                let files_stay_desc = t("mesh-files-stay-desc");
                peer_rows.push(setting_row_described(
                    &files_stay,
                    &files_stay_desc,
                    Box::new(
                        Button::new(&cancel_action, &t("mesh-cancel"), Some(&cancel_action))
                            .content_sized(),
                    ),
                ));
            }
            sections.push(card_with_action(&peer.label, remove, rows(peer_rows)));
        }
        if shows_advanced_services(self.config.device_role) {
            let services_title = t("mesh-services-title");
            let svc_relay = t("mesh-svc-relay");
            let svc_relay_desc = t("mesh-svc-relay-desc");
            let svc_remote = t("mesh-svc-remote");
            let svc_remote_desc = t("mesh-svc-remote-desc");
            let svc_apps = t("mesh-svc-apps");
            let svc_apps_desc = t("mesh-svc-apps-desc");
            let svc_jobs = t("mesh-svc-jobs");
            let svc_jobs_desc = t("mesh-svc-jobs-desc");
            sections.push(card(
                &services_title,
                rows(vec![
                    setting_row_described(
                        &svc_relay,
                        &svc_relay_desc,
                        Box::new(
                            crate::actions::device_mesh_bool_binding("inbound-relay").map_or_else(
                                || {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.inbound_relay,
                                    ))
                                    .with_id("device_mesh.inbound_relay")
                                    .with_enabled(false)
                                },
                                |binding| {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.inbound_relay,
                                    ))
                                    .with_id("device_mesh.inbound_relay")
                                    .on_action(binding)
                                },
                            ),
                        ),
                    ),
                    setting_row_described(
                        &svc_remote,
                        &svc_remote_desc,
                        Box::new(
                            crate::actions::device_mesh_bool_binding("remote-control").map_or_else(
                                || {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.remote_control_relay,
                                    ))
                                    .with_id("device_mesh.remote_relay")
                                    .with_enabled(false)
                                },
                                |binding| {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.remote_control_relay,
                                    ))
                                    .with_id("device_mesh.remote_relay")
                                    .on_action(binding)
                                },
                            ),
                        ),
                    ),
                    setting_row_described(
                        &svc_apps,
                        &svc_apps_desc,
                        Box::new(
                            crate::actions::device_mesh_bool_binding("app-serving").map_or_else(
                                || {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.app_serving,
                                    ))
                                    .with_id("device_mesh.app_serving")
                                    .with_enabled(false)
                                },
                                |binding| {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.app_serving,
                                    ))
                                    .with_id("device_mesh.app_serving")
                                    .on_action(binding)
                                },
                            ),
                        ),
                    ),
                    setting_row_described(
                        &svc_jobs,
                        &svc_jobs_desc,
                        Box::new(
                            crate::actions::device_mesh_bool_binding("compute").map_or_else(
                                || {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.compute,
                                    ))
                                    .with_enabled(false)
                                },
                                |binding| {
                                    Toggle::from_state(&ui::LocalState::new(
                                        self.config.services.compute,
                                    ))
                                    .with_id("device_mesh.compute")
                                    .on_action(binding)
                                },
                            ),
                        ),
                    ),
                ]),
            ));
        }

        cards(sections)
    }

    fn persist(&mut self) {
        if self.config_path == std::path::Path::new(DEFAULT_CONFIG) {
            if self.apply_child.is_some() {
                self.apply_queued = true;
                self.load_status = "Saving latest changes…".into();
            } else {
                self.start_apply();
            }
            return;
        }
        self.load_status = match self.config.save(&self.config_path) {
            Ok(()) => "Saved".to_string(),
            Err(error) => format!("Not saved: {error}"),
        };
    }

    fn spawn_apply(&self) -> Result<bounded_command::AsyncCommand, String> {
        let request = serde_json::json!({
            "identity_root": identity_root(),
            "role": match self.config.device_role {
                DeviceRole::Interactive => "interactive",
                DeviceRole::Hybrid => "hybrid",
                DeviceRole::AlwaysOnServer => "always-on",
            },
            "workspace_ids": self.config.workspaces.iter().map(|workspace| workspace.id).collect::<Vec<_>>(),
            "replication": match self.common_replication() {
                ReplicationIntent::WorkspaceOnly => "workspace-only",
                ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => "on-demand",
                ReplicationIntent::FullWorkspace => "full",
            },
            "inbound_relay": self.config.services.inbound_relay,
            "remote_control_relay": self.config.services.remote_control_relay,
            "app_serving": self.config.services.app_serving,
            "compute": self.config.services.compute,
        });
        let mut command = Command::new("pkexec");
        command.args([CONTROL_HELPER, "apply"]);
        bounded_command::spawn_prepared(
            command,
            Some(request.to_string().as_bytes()),
            COMMAND_OUTPUT_LIMIT,
            APPLY_TIMEOUT,
        )
        .map_err(|error| format!("could not start system setup: {error}"))
    }

    fn start_apply(&mut self) {
        match self.spawn_apply() {
            Ok(child) => {
                self.apply_child = Some(child);
                self.load_status = "Saving changes…".into();
            }
            Err(error) => self.load_status = format!("Not saved: {error}"),
        }
    }

    fn approve_enrollment(&mut self, target: PairingTarget) {
        if self.pairing_child.is_some() {
            return;
        }
        let Some(candidate) = self.pairing_candidate.clone() else {
            self.pairing_target = target;
            let discover_command = match target {
                PairingTarget::HomePeer => "discover",
                PairingTarget::PersonalDevice => "discover-device",
            };
            let mut command = Command::new("/usr/bin/tilo-enroll");
            command.args([discover_command, "--timeout", "15"]);
            self.load_status = match bounded_command::spawn_prepared(
                command,
                None,
                COMMAND_OUTPUT_LIMIT,
                DISCOVERY_TIMEOUT,
            ) {
                Ok(child) => {
                    self.pairing_child = Some(child);
                    self.pairing_discovery = true;
                    match target {
                        PairingTarget::HomePeer => "Looking for your new home server…".into(),
                        PairingTarget::PersonalDevice => "Looking for your new device…".into(),
                    }
                }
                Err(error) => format!("Could not start nearby discovery: {error}"),
            };
            self.structure_dirty = true;
            return;
        };
        if self.pairing_target != target {
            self.pairing_candidate = None;
            self.approve_enrollment(target);
            return;
        }
        let mut command = Command::new("pkexec");
        match target {
            PairingTarget::HomePeer => {
                command
                    .args([CONTROL_HELPER, "approve-enrollment", "--identity-root"])
                    .arg(identity_root());
            }
            PairingTarget::PersonalDevice => {
                let identity_dir = match runtime_identity::identity_dir_from_env() {
                    Ok(path) => path,
                    Err(error) => {
                        self.load_status = format!("Could not find your identity: {error}");
                        self.structure_dirty = true;
                        return;
                    }
                };
                command
                    .args([CONTROL_HELPER, "approve-device", "--identity-root"])
                    .arg(identity_root())
                    .arg("--identity-dir")
                    .arg(identity_dir);
            }
        }
        command
            .args(["--expected-device", &candidate.device_node_id])
            .args(["--expected-service", &candidate.relay_service_node_id]);
        let ids = if self.config.workspaces.is_empty() {
            self.discovered_workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>()
        } else {
            self.config
                .workspaces
                .iter()
                .map(|workspace| workspace.id)
                .collect()
        };
        if target == PairingTarget::HomePeer {
            for id in ids {
                command.args(["--workspace-id", &id.to_string()]);
            }
        }
        self.load_status = match bounded_command::spawn_prepared(
            command,
            None,
            COMMAND_OUTPUT_LIMIT,
            ENROLLMENT_TIMEOUT,
        ) {
            Ok(child) => {
                self.pairing_child = Some(child);
                self.pairing_discovery = false;
                format!(
                    "Approving {} with code {}…",
                    candidate.label, candidate.verification_code
                )
            }
            Err(error) => format!("Could not start pairing: {error}"),
        };
        self.structure_dirty = true;
    }

    fn start_home_peer_command(
        &mut self,
        peer: &tilo_relay_protocol::HomePeerRegistration,
        action: &str,
        label: Option<&str>,
    ) {
        if self.management_child.is_some() {
            return;
        }
        let mut command = Command::new(RELAY_ADMIN);
        command
            .arg(action)
            .arg("--identity-root")
            .arg(identity_root())
            .args(["--device", &peer.device_node_id])
            .args(["--service", &peer.relay_service_node_id]);
        if let Some(label) = label {
            command.args(["--label", label]);
        }
        match bounded_command::spawn_prepared(
            command,
            None,
            COMMAND_OUTPUT_LIMIT,
            MANAGEMENT_TIMEOUT,
        ) {
            Ok(child) => {
                self.management_child = Some(child);
                self.management_device = Some(peer.device_node_id.clone());
                self.home_peer_health
                    .insert(peer.device_node_id.clone(), "Connecting…".into());
            }
            Err(error) => {
                self.home_peer_health
                    .insert(peer.device_node_id.clone(), format!("Offline · {error}"));
            }
        }
        self.structure_dirty = true;
    }

    fn refresh_one_home_peer(&mut self) {
        if self.management_child.is_some() {
            return;
        }
        let pending = identity_root().join("pending-home-peer-decommissions.json");
        if std::fs::read(&pending)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
            .is_some_and(|pending| !pending.is_empty())
        {
            let mut command = Command::new(RELAY_ADMIN);
            command
                .arg("home-peer-retry-removals")
                .arg("--identity-root")
                .arg(identity_root());
            if let Ok(child) = bounded_command::spawn_prepared(
                command,
                None,
                COMMAND_OUTPUT_LIMIT,
                MANAGEMENT_TIMEOUT,
            ) {
                self.management_child = Some(child);
                self.management_device = Some("__pending__".into());
            }
            return;
        }
        if self.home_peers.is_empty() {
            return;
        }
        let index = self.next_peer_health % self.home_peers.len();
        self.next_peer_health = self.next_peer_health.wrapping_add(1);
        let peer = self.home_peers.get(index).cloned();
        if let Some(peer) = peer {
            self.start_home_peer_command(&peer, "home-peer-status", None);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "polling keeps each child-process completion transition together so success, retry, error text, and UI rebuild state cannot drift apart"
    )]
    fn poll_background_tasks(&mut self) {
        let apply_result = self
            .apply_child
            .as_mut()
            .and_then(bounded_command::AsyncCommand::try_wait);
        match apply_result {
            Some(Ok(output)) => {
                self.apply_child = None;
                if output.status.success() {
                    if self.apply_queued {
                        self.apply_queued = false;
                        self.start_apply();
                    } else {
                        self.load_status = "Saved".into();
                    }
                } else {
                    self.apply_queued = false;
                    self.load_status = format!(
                        "Not saved: {}",
                        command_error(&output.stderr, "administrator approval was not granted")
                    );
                }
                self.structure_dirty = true;
            }
            Some(Err(error)) => {
                self.apply_child = None;
                self.apply_queued = false;
                self.load_status = format!("Not saved: {error}");
                self.structure_dirty = true;
            }
            None => {}
        }

        let pairing_result = self
            .pairing_child
            .as_mut()
            .and_then(bounded_command::AsyncCommand::try_wait);
        match pairing_result {
            Some(Ok(output)) => {
                self.pairing_child = None;
                if output.status.success() && self.pairing_discovery {
                    match serde_json::from_slice::<PairCandidate>(&output.stdout) {
                        Ok(candidate) => {
                            self.load_status = format!(
                                "Found {}. Confirm code {} on both devices, then approve.",
                                candidate.label, candidate.verification_code
                            );
                            self.pairing_candidate = Some(candidate);
                        }
                        Err(_) => {
                            self.load_status =
                                "Nearby discovery returned an invalid identity".into();
                        }
                    }
                } else if output.status.success() {
                    self.registered_home_peers = registered_home_peer_count();
                    self.refresh_runtime_status();
                    self.load_status = match self.pairing_target {
                        PairingTarget::HomePeer => "Home server paired".into(),
                        PairingTarget::PersonalDevice => {
                            match crate::device_fleet::list() {
                                Ok(devices) => {
                                    self.personal_devices = devices;
                                    self.personal_device_error = None;
                                }
                                Err(error) => self.personal_device_error = Some(error.to_string()),
                            }
                            "Personal device added".into()
                        }
                    };
                    self.pairing_candidate = None;
                } else {
                    self.load_status = format!(
                        "Device was not paired: {}. Choose the pairing action to retry.",
                        command_error(&output.stderr, "the pairing window closed or was cancelled")
                    );
                }
                self.pairing_discovery = false;
                self.structure_dirty = true;
            }
            Some(Err(error)) => {
                self.pairing_child = None;
                self.load_status =
                    format!("Pairing stopped: {error}. Choose the pairing action to retry.");
                self.structure_dirty = true;
            }
            None => {}
        }

        let management_result = self
            .management_child
            .as_mut()
            .and_then(bounded_command::AsyncCommand::try_wait);
        match management_result {
            Some(Ok(output)) => {
                self.management_child = None;
                let device = self.management_device.take().unwrap_or_default();
                if output.status.success() {
                    if let Ok(response) =
                        serde_json::from_slice::<serde_json::Value>(&output.stdout)
                    {
                        let ready = response
                            .get("files_ready")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let removed = response
                            .get("decommissioned")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        self.home_peer_health.insert(
                            device.clone(),
                            if removed {
                                "Removed".into()
                            } else if ready {
                                "Online · Files ready".into()
                            } else {
                                "Online · Syncing files".into()
                            },
                        );
                        let now_ms = ui::time::wall()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map_or(0, |duration| {
                                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                            });
                        self.home_peer_last_seen.insert(device, now_ms);
                        save_home_peer_last_seen(&self.home_peer_last_seen);
                        self.home_peers = registered_home_peers();
                        self.registered_home_peers = self.home_peers.len();
                    }
                } else {
                    let detail = command_error(&output.stderr, "not reachable");
                    let label = self.home_peer_last_seen.get(&device).map_or_else(
                        || format!("Offline · {detail}"),
                        |last_seen| last_seen_label(*last_seen),
                    );
                    self.home_peer_health.insert(device, label);
                }
                self.structure_dirty = true;
            }
            Some(Err(error)) => {
                self.management_child = None;
                if let Some(device) = self.management_device.take() {
                    self.home_peer_health
                        .insert(device, format!("Offline · {error}"));
                }
                self.structure_dirty = true;
            }
            None => {}
        }
    }
}

impl Default for DeviceMeshPane {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(DeviceMeshPane::new())
}

impl Widget for DeviceMeshPane {
    ui::provides!(dyn ui::undo::UndoPoints);

    #[allow(
        clippy::too_many_lines,
        reason = "one synchronization pass consumes the pane's related role, replication, pairing, device-revocation, and server-management controls atomically"
    )]
    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.poll_background_tasks();
        if let Some(command_state) = &self.command_state
            && let Some(operation) = command_state.get()
        {
            command_state.set(None);
            self.apply_command(&operation);
        }
        if self.needs_seed {
            for peer in &self.home_peers {
                let _ = state.put(
                    root,
                    peer_key("name", &peer.device_node_id),
                    peer.label.clone(),
                );
                let _ = state.put(root, peer_key("rename", &peer.device_node_id), false);
                let _ = state.put(root, peer_key("remove", &peer.device_node_id), false);
                let _ = state.put(root, peer_key("cancel_remove", &peer.device_node_id), false);
            }
            for device in &self.personal_devices {
                let _ = state.put(root, personal_device_key("revoke", &device.key), false);
                let _ = state.put(root, personal_device_key("cancel", &device.key), false);
                let _ = state.put(root, personal_device_key("reason", &device.key), "Retired");
            }
            let _ = state.put(root, APPROVE_ENROLLMENT_KEY, false);
            let _ = state.put(root, APPROVE_PERSONAL_DEVICE_KEY, false);
            self.needs_seed = false;
        }

        // Device role, relay services, and workspace residency never enter
        // the collaborative document. Their controls emit typed local effect
        // operations and `apply_command` persists directly to RelayConfig.
        if read_bool(state, root, APPROVE_ENROLLMENT_KEY) == Some(true) {
            let _ = state.put(root, APPROVE_ENROLLMENT_KEY, false);
            if self.pairing_child.is_none() {
                self.approve_enrollment(PairingTarget::HomePeer);
            }
        }
        if read_bool(state, root, APPROVE_PERSONAL_DEVICE_KEY) == Some(true) {
            let _ = state.put(root, APPROVE_PERSONAL_DEVICE_KEY, false);
            if self.pairing_child.is_none() {
                self.approve_enrollment(PairingTarget::PersonalDevice);
            }
        }
        for device in self.personal_devices.clone() {
            let revoke_key = personal_device_key("revoke", &device.key);
            let cancel_key = personal_device_key("cancel", &device.key);
            if read_bool(state, root, &cancel_key) == Some(true) {
                let _ = state.put(root, &cancel_key, false);
                self.personal_remove_armed.remove(&device.key);
                self.structure_dirty = true;
            }
            if read_bool(state, root, &revoke_key) == Some(true) {
                let _ = state.put(root, &revoke_key, false);
                if self.personal_remove_armed.remove(&device.key) {
                    let reason = read_str(state, root, personal_device_key("reason", &device.key))
                        .map_or(RevocationReason::Retired, |label| revocation_reason(&label));
                    match crate::device_fleet::revoke(device.key, reason) {
                        Ok(()) => {
                            self.personal_devices = crate::device_fleet::list().unwrap_or_default();
                            self.personal_device_error = None;
                            self.load_status =
                                format!("{} can no longer access your workspaces", device.label);
                        }
                        Err(error) => {
                            self.personal_device_error = Some(error.to_string());
                        }
                    }
                } else if !device.current && !device.revoked {
                    self.personal_remove_armed.insert(device.key);
                }
                self.structure_dirty = true;
            }
        }
        for peer in self.home_peers.clone() {
            let rename_key = peer_key("rename", &peer.device_node_id);
            if read_bool(state, root, &rename_key) == Some(true) {
                let _ = state.put(root, &rename_key, false);
                if let Some(label) = read_str(state, root, peer_key("name", &peer.device_node_id))
                    && tilo_relay_protocol::valid_home_peer_label(&label)
                {
                    self.start_home_peer_command(&peer, "home-peer-rename", Some(label.trim()));
                }
            }
            let remove_key = peer_key("remove", &peer.device_node_id);
            let cancel_remove_key = peer_key("cancel_remove", &peer.device_node_id);
            if read_bool(state, root, &cancel_remove_key) == Some(true) {
                let _ = state.put(root, &cancel_remove_key, false);
                self.remove_armed.remove(&peer.device_node_id);
                self.structure_dirty = true;
            }
            if read_bool(state, root, &remove_key) == Some(true) {
                let _ = state.put(root, &remove_key, false);
                if self.remove_armed.remove(&peer.device_node_id) {
                    self.start_home_peer_command(&peer, "home-peer-remove", None);
                } else {
                    self.remove_armed.insert(peer.device_node_id.clone());
                    self.load_status = format!(
                        "Review the file-copy warning, then choose Revoke & remove to stop {} permanently",
                        peer.label
                    );
                    self.structure_dirty = true;
                }
            }
        }
        if ui::time::now() >= self.next_status_refresh {
            self.refresh_runtime_status();
            self.refresh_one_home_peer();
        }
        if let Some(composed) = &mut self.composed {
            composed.sync(state, root);
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
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            composed.apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
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
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(composed) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, *child));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|child| child.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own || self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.apply_child.is_some()
            || self.pairing_child.is_some()
            || self.management_child.is_some()
        {
            Some(ui::time::now() + PROCESS_POLL_INTERVAL)
        } else {
            shows_advanced_services(self.config.device_role).then_some(self.next_status_refresh)
        }
    }

    fn name(&self) -> &'static str {
        "DeviceMeshPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for DeviceMeshPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "devices-sync",
            "Devices & Sync",
            "Change device mesh settings",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use settings_store::roaming_provenance::{SCHEMA_VERSION, SettingResolution, WifiResolution};
    use tilo_relay_protocol::WorkspaceBinding;

    fn test_ids(widget: &dyn Widget) -> Vec<String> {
        fn walk(widget: &dyn Widget, ids: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                ids.push(id.to_string());
            }
            for child in widget.children() {
                walk(child, ids);
            }
        }
        let mut ids = Vec::new();
        walk(widget, &mut ids);
        ids
    }

    /// Continuity controls, conflict summaries, refused future provenance and redacted audit rows.
    #[test]
    fn continuity_controls_are_typed_audited_and_refuse_future_provenance() {
        // Case `continuity_ui_exposes_master_and_every_typed_capability_control`.
        {
            let dir = tempfile::tempdir().unwrap();
            let pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            let composition = pane.build_composition();
            let ids = test_ids(composition.as_ref());

            assert!(ids.iter().any(|id| id == CONTINUITY_ENABLED_KEY));
            for capability in Capability::ALL {
                let key = continuity_key(capability);
                assert!(ids.iter().any(|id| id == &key), "missing {key}");
            }
        }

        // Case `roaming_conflict_summaries_name_deterministic_winners_without_secrets`.
        {
            let origin = |id: &str, label: &str, counter| Origin {
                device_id: id.into(),
                device_label: label.into(),
                counter,
            };
            let projection = RoamingProvenance {
                schema_version: SCHEMA_VERSION,
                generated_unix_ms: 1,
                settings: vec![SettingResolution {
                    key: "tilo.appearance.theme".into(),
                    winner: origin("b", "Laptop", 8),
                    alternatives: vec![origin("a", "Desktop", 7)],
                }],
                wifi: vec![WifiResolution {
                    network_id: "ab".repeat(32),
                    ssid: "Home".into(),
                    security: "WPA2".into(),
                    winner: origin("b", "Laptop", 9),
                    alternatives: Vec::new(),
                }],
            };
            let summaries = provenance_summaries(&projection);
            assert_eq!(
                summaries,
                vec![
                    (
                        "tilo.appearance.theme".into(),
                        "Resolved to Laptop · revision 8; differs from Desktop".into()
                    ),
                    (
                        "Wi-Fi · Home".into(),
                        "Sealed WPA2 profile. From Laptop · revision 9".into()
                    )
                ]
            );
            let encoded = format!("{summaries:?}");
            assert!(!encoded.contains("password"));
            assert!(!encoded.contains("sealed_secret"));
        }

        // Case `future_provenance_is_refused_instead_of_partially_rendered`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir
                .path()
                .join(settings_store::roaming_provenance::FILE_NAME);
            std::fs::write(&path, r#"{"schema_version":99}"#).unwrap();
            let (projection, status, _) = load_roaming_provenance(Some(&path));
            assert!(projection.is_none());
            assert!(
                status
                    .unwrap()
                    .contains("unsupported roaming provenance schema 99")
            );
        }

        // Case `continuity_edits_persist_locally_and_append_redacted_audit_rows`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetService {
                service: "continuity.enabled".into(),
                enabled: true,
            });
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetService {
                service: "continuity.clipboard".into(),
                enabled: true,
            });

            let saved = ContinuityPrivacy::load_at(&dir.path().join("settings.json"));
            assert!(saved.enabled);
            assert!(saved.effective(Capability::Clipboard));
            assert_eq!(saved.audit.len(), 2);
            let encoded = serde_json::to_string(&saved.audit).unwrap();
            for forbidden in ["payload", "token", "address", "file_name"] {
                assert!(!encoded.contains(forbidden));
            }
        }
    }

    /// Relay config shape, honest missing config, file policy intents and non-blocking coalesced applies.
    #[test]
    fn relay_config_edits_are_real_honest_and_coalesced() {
        // Case `edits_write_the_real_relay_config_shape`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut config = RelayConfig::default();
            config.workspaces.push(WorkspaceBinding {
                id: 7,
                pin: true,
                replication: ReplicationIntent::OnDemand,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
            });
            config.save(&path).unwrap();

            let mut pane = DeviceMeshPane::with_path(&path);
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetRole {
                role: ROLE_ACTIVE.into(),
            });
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetReplication {
                replication: REPLICATION_FULL.into(),
            });
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetService {
                service: "remote_control_relay".into(),
                enabled: false,
            });

            let saved = RelayConfig::load(&path).unwrap();
            assert_eq!(saved.device_role, DeviceRole::Hybrid);
            assert_eq!(
                saved.workspaces[0].replication,
                ReplicationIntent::FullWorkspace
            );
            assert!(!saved.services.remote_control_relay);
        }

        // Case `missing_config_is_honest_and_not_created_until_an_edit`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut pane = DeviceMeshPane::with_path(&path);
            assert!(pane.load_status.contains("Not configured"));
            assert_eq!(pane.config.device_role, DeviceRole::Interactive);
            assert_eq!(
                pane.config.services,
                tilo_relay_protocol::ServiceCapabilities::default()
            );
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(!path.exists());
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetRole {
                role: ROLE_ACTIVE.into(),
            });
            assert!(path.exists());
        }

        // Case `file_policy_labels_map_to_the_existing_replication_intents`.
        {
            assert_eq!(
                replication_from_label(REPLICATION_WORKSPACE),
                Some(ReplicationIntent::WorkspaceOnly)
            );
            assert_eq!(
                replication_from_label(REPLICATION_ON_DEMAND),
                Some(ReplicationIntent::OnDemand)
            );
            assert_eq!(
                replication_from_label(REPLICATION_FULL),
                Some(ReplicationIntent::FullWorkspace)
            );
            assert_eq!(
                replication_label(&ReplicationIntent::OnDemand),
                REPLICATION_ON_DEMAND
            );
            assert_eq!(
                replication_label(&ReplicationIntent::FullWorkspace),
                REPLICATION_FULL
            );
            assert!(
                replication_description(&ReplicationIntent::OnDemand)
                    .contains("preview thumbnails")
            );
            assert!(
                replication_description(&ReplicationIntent::FullWorkspace)
                    .contains("full file copies")
            );
        }

        // Case `active_config_apply_is_polled_without_blocking_and_coalesces_edits`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            pane.config_path = PathBuf::from(DEFAULT_CONFIG);
            let mut command = Command::new("sleep");
            command.arg("2");
            pane.apply_child = Some(
                bounded_command::spawn_prepared(command, None, 64, Duration::from_secs(5)).unwrap(),
            );

            let started = ui::time::now();
            pane.persist();
            pane.poll_background_tasks();

            assert!(started.elapsed() < Duration::from_millis(250));
            assert!(pane.apply_child.is_some());
            assert!(pane.apply_queued);
            pane.apply_child = None;
        }
    }

    /// Serving roles, rebuild requests, always-available outcomes and replicated-edit immunity.
    #[test]
    fn device_roles_and_availability_stay_local() {
        // Case `advanced_services_only_appear_for_devices_that_can_serve`.
        {
            assert!(!shows_advanced_services(DeviceRole::Interactive));
            assert!(shows_advanced_services(DeviceRole::Hybrid));
            assert!(shows_advanced_services(DeviceRole::AlwaysOnServer));
        }

        // Case `changing_between_personal_and_serving_roles_requests_a_rebuild`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let config = RelayConfig {
                device_role: DeviceRole::Interactive,
                ..RelayConfig::default()
            };
            config.save(&path).unwrap();
            let mut pane = DeviceMeshPane::with_path(&path);
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(!pane.take_structure_dirty());

            pane.apply_command(&crate::actions::DeviceMeshOperation::SetRole {
                role: ROLE_ACTIVE.into(),
            });

            assert!(pane.take_structure_dirty());
            assert_eq!(pane.config.device_role, DeviceRole::Hybrid);
        }

        // Case `always_available_is_one_outcome_that_enables_full_sync`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut config = RelayConfig {
                device_role: DeviceRole::Interactive,
                services: tilo_relay_protocol::ServiceCapabilities::default(),
                ..RelayConfig::default()
            };
            config.workspaces.push(WorkspaceBinding {
                id: 7,
                pin: false,
                replication: ReplicationIntent::OnDemand,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
            });
            config.save(&path).unwrap();

            let mut pane = DeviceMeshPane::with_path(&path);
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetRole {
                role: ROLE_SERVER.into(),
            });

            let saved = RelayConfig::load(&path).unwrap();
            assert_eq!(saved.device_role, DeviceRole::AlwaysOnServer);
            assert!(saved.services.workspace_replication);
            assert!(saved.services.full_content_replication);
            assert!(saved.services.inbound_relay);
            assert!(saved.workspaces[0].pin);
            assert_eq!(
                saved.workspaces[0].replication,
                ReplicationIntent::FullWorkspace
            );
        }

        // Case `replicated_document_edits_cannot_change_local_device_role`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let config = RelayConfig {
                device_role: DeviceRole::Interactive,
                ..RelayConfig::default()
            };
            config.save(&path).unwrap();
            let mut pane = DeviceMeshPane::with_path(&path);
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);

            doc.put(&automerge::ROOT, "device_mesh.role", ROLE_SERVER)
                .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);

            assert_eq!(pane.config.device_role, DeviceRole::Interactive);
            assert_eq!(
                RelayConfig::load(&path).unwrap().device_role,
                DeviceRole::Interactive
            );
        }
    }

    /// Peer health, workspace bindings, fresh metrics and complete residency.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn device_health_and_workspace_status_reflect_fresh_facts() {
        // Case `peer_health_is_a_compact_visual_state_with_detail_beside_it`.
        {
            assert_eq!(peer_health_badge("Online · Files ready"), "Online");
            assert_eq!(peer_health_badge("Online · Syncing files"), "Syncing");
            assert_eq!(peer_health_badge("Offline · Last seen 4 h ago"), "Offline");
            assert_eq!(peer_health_badge("Checking…"), "Checking");
        }

        // Case `discovered_workspace_toggle_updates_the_real_binding_set`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut pane = DeviceMeshPane::with_path(&path);
            pane.discovered_workspaces = vec![DiscoveredWorkspace {
                id: 44,
                name: "Studio".into(),
            }];
            pane.apply_command(&crate::actions::DeviceMeshOperation::SetWorkspace {
                id: 44,
                enabled: true,
            });
            assert!(
                pane.config
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == 44)
            );
            assert!(
                RelayConfig::load(&path)
                    .unwrap()
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == 44)
            );
        }

        // Case `fresh_daemon_metrics_drive_online_status`.
        {
            let dir = tempfile::tempdir().unwrap();
            let config = RelayConfig {
                state_root: dir.path().to_path_buf(),
                device_role: DeviceRole::Hybrid,
                ..RelayConfig::default()
            };
            let metrics = tilo_relay_protocol::metrics::MetricsSnapshot {
                deltas_received: 42,
                ..Default::default()
            };
            tilo_relay_protocol::metrics::save_metrics(&config.state_root, &metrics).unwrap();
            let modified = std::fs::metadata(
                config
                    .state_root
                    .join(tilo_relay_protocol::metrics::METRICS_FILE),
            )
            .unwrap()
            .modified()
            .unwrap();

            assert_eq!(
                RelayRuntimeStatus::read(&config, modified + Duration::from_secs(1)),
                RelayRuntimeStatus::Online
            );
            assert_eq!(
                RelayRuntimeStatus::read(
                    &config,
                    modified + STATUS_FRESHNESS + Duration::from_secs(1)
                ),
                RelayRuntimeStatus::Offline
            );
        }

        // Case `always_on_status_requires_fresh_complete_file_residency`.
        {
            let dir = tempfile::tempdir().unwrap();
            let proof = dir.path().join("full-replica.json");
            let config = RelayConfig {
                state_root: dir.path().to_path_buf(),
                full_replica_status: proof.clone(),
                device_role: DeviceRole::AlwaysOnServer,
                services: tilo_relay_protocol::ServiceCapabilities::personal_server(),
                ..RelayConfig::default()
            };
            tilo_relay_protocol::metrics::save_metrics(
                &config.state_root,
                &tilo_relay_protocol::metrics::MetricsSnapshot::default(),
            )
            .unwrap();
            // Observe at the snapshot's own modification time, not the wall
            // clock after the save: the atomic write stamps the file before its
            // fsync, and a slow fsync would otherwise age the snapshot past
            // STATUS_FRESHNESS before the first read.
            let now = std::fs::metadata(
                config
                    .state_root
                    .join(tilo_relay_protocol::metrics::METRICS_FILE),
            )
            .unwrap()
            .modified()
            .unwrap();
            let now_ms = now
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis();
            std::fs::write(
                &proof,
                format!(
                    r#"{{"state":"ready","full_content_replication":true,"updated_at_ms":{now_ms}}}"#
                ),
            )
            .unwrap();
            assert_eq!(
                RelayRuntimeStatus::read(&config, now),
                RelayRuntimeStatus::Ready
            );
            std::fs::write(
                &proof,
                r#"{"state":"syncing","full_content_replication":false,"updated_at_ms":0}"#,
            )
            .unwrap();
            assert_eq!(
                RelayRuntimeStatus::read(&config, now),
                RelayRuntimeStatus::Syncing
            );
        }
    }

    /// Reviewed removal, armed decommission and single-flight pairing.
    #[test]
    fn device_removal_and_pairing_are_armed_and_single_flight() {
        // Case `home_server_removal_requires_review_and_can_be_cancelled`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            pane.home_peers = vec![tilo_relay_protocol::HomePeerRegistration {
                schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                device_node_id: "11".repeat(32),
                relay_service_node_id: "22".repeat(32),
                relay_endpoint: None,
                label: "Studio NAS".into(),
                user_public_key: "33".repeat(32),
                device_cert: None,
                relay_workspace_ids: vec![7],
            }];
            let device = pane.home_peers[0].device_node_id.clone();
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);

            doc.put(&automerge::ROOT, peer_key("remove", &device), true)
                .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(pane.remove_armed.contains(&device));
            assert!(pane.load_status.contains("file-copy warning"));
            assert!(pane.management_child.is_none());

            doc.put(&automerge::ROOT, peer_key("cancel_remove", &device), true)
                .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(!pane.remove_armed.contains(&device));
            assert!(pane.management_child.is_none());
        }

        // Case `personal_device_decommission_is_armed_before_any_revocation`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            let key = [42; 32];
            pane.personal_devices = vec![crate::device_fleet::DeviceView {
                label: "Old laptop".into(),
                key,
                fingerprint: "2A 2A 2A 2A 2A 2A".into(),
                current: false,
                revoked: false,
                can_host: false,
            }];
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);

            doc.put(&automerge::ROOT, personal_device_key("revoke", &key), true)
                .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);

            assert!(pane.personal_remove_armed.contains(&key));
            assert_eq!(
                read_str(&doc, &automerge::ROOT, personal_device_key("reason", &key)).as_deref(),
                Some("Retired")
            );
        }

        // Case `pairing_process_transitions_to_success_and_allows_another_pairing`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            // The child's own timeout bounds the wait; how quickly a loaded
            // machine schedules `true` is not what this test is about.
            let timeout = Duration::from_secs(30);
            pane.pairing_child = Some(
                bounded_command::spawn_prepared(Command::new("true"), None, 64, timeout).unwrap(),
            );
            let deadline = ui::time::now() + timeout;
            while pane.pairing_child.is_some() && ui::time::now() < deadline {
                pane.poll_background_tasks();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(pane.pairing_child.is_none());
            assert_eq!(pane.load_status, "Home server paired");
        }

        // Case `duplicate_pair_action_is_ignored_while_pairing_is_active`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
            let mut command = Command::new("sleep");
            command.arg("2");
            pane.pairing_child = Some(
                bounded_command::spawn_prepared(command, None, 64, Duration::from_secs(5)).unwrap(),
            );
            pane.approve_enrollment(PairingTarget::HomePeer);
            assert!(pane.pairing_child.is_some());
            pane.pairing_child = None;
        }
    }
}
