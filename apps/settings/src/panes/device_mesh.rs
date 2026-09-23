//! Devices & Sync — configure seamless continuation and always-on sync.
//!
//! The pane reads and writes the same `RelayConfig` consumed by `tilo-relay`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};
use ui::doc::{read_bool, read_str};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tilo_relay::{DeviceRole, RelayConfig, ReplicationIntent};
use tilo_user_identity::RevocationReason;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::continuity::{Capability, ContinuityPrivacy};
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
const ROLE_OPTIONS: &[&str] = &[ROLE_PERSONAL, ROLE_ACTIVE, ROLE_SERVER];
const REPLICATION_ON_DEMAND: &str = "On demand";
const REPLICATION_WORKSPACE: &str = "Workspace state";
const REPLICATION_FULL: &str = "Full workspace copy";
const REPLICATION_OPTIONS: &[&str] = &[
    REPLICATION_ON_DEMAND,
    REPLICATION_WORKSPACE,
    REPLICATION_FULL,
];
const REVOKE_REASONS: &[&str] = &["Retired", "Lost", "Stolen", "Compromised"];

fn role_label(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Interactive => ROLE_PERSONAL,
        DeviceRole::Hybrid => ROLE_ACTIVE,
        DeviceRole::AlwaysOnServer => ROLE_SERVER,
    }
}

fn role_from_label(label: &str) -> Option<DeviceRole> {
    match label {
        ROLE_PERSONAL => Some(DeviceRole::Interactive),
        ROLE_ACTIVE => Some(DeviceRole::Hybrid),
        ROLE_SERVER => Some(DeviceRole::AlwaysOnServer),
        _ => None,
    }
}

fn replication_label(intent: &ReplicationIntent) -> &'static str {
    match intent {
        ReplicationIntent::WorkspaceOnly => REPLICATION_WORKSPACE,
        ReplicationIntent::FullWorkspace => REPLICATION_FULL,
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => REPLICATION_ON_DEMAND,
    }
}

fn replication_from_label(label: &str) -> Option<ReplicationIntent> {
    match label {
        REPLICATION_ON_DEMAND => Some(ReplicationIntent::OnDemand),
        REPLICATION_WORKSPACE => Some(ReplicationIntent::WorkspaceOnly),
        REPLICATION_FULL => Some(ReplicationIntent::FullWorkspace),
        _ => None,
    }
}

fn replication_description(intent: &ReplicationIntent) -> &'static str {
    match intent {
        ReplicationIntent::WorkspaceOnly => {
            "Open workspaces and app progress sync here without keeping workspace files."
        }
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => {
            "File names, metadata, and preview thumbnails stay ready. File contents download when opened."
        }
        ReplicationIntent::FullWorkspace => {
            "Open workspaces, app progress, and full file copies stay synced on this device."
        }
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
        let metrics_path = config.state_root.join(tilo_relay::store::METRICS_FILE);
        let fresh = std::fs::metadata(metrics_path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified).unwrap_or(Duration::ZERO) <= STATUS_FRESHNESS
            });
        if !fresh {
            return Self::Offline;
        }
        match tilo_relay::store::load_metrics(&config.state_root) {
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

    const fn label(self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Online => "Online",
            Self::Syncing => "Syncing",
            Self::Ready => "Ready",
        }
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
    home_peers: Vec<tilo_relay::HomePeerRegistration>,
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
    let path = dirs::config_dir().map_or_else(
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
                        .and_then(|room| tilo_relay::room_workspace_id(room).ok())
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

fn registered_home_peers() -> Vec<tilo_relay::HomePeerRegistration> {
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
    let mut encoded = String::with_capacity(64);
    for byte in device {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("device_mesh.personal_device.{action}.{encoded}.edit")
}

fn continuity_key(capability: Capability) -> String {
    format!("device_mesh.continuity.{}", capability.key())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn origin_summary(origin: &Origin) -> String {
    format!("{} · revision {}", origin.device_label, origin.counter)
}

fn resolution_summary(winner: &Origin, alternatives: &[Origin]) -> String {
    if alternatives.is_empty() {
        return format!("From {}", origin_summary(winner));
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
        format!(" and {remainder} more")
    };
    format!(
        "Resolved to {}; differs from {others}{suffix}",
        origin_summary(winner)
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
                format!("Wi-Fi · {}", wifi.ssid),
                format!(
                    "Sealed {} profile. {}",
                    wifi.security,
                    resolution_summary(&wifi.winner, &wifi.alternatives)
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
            Some(format!(
                "Roaming provenance is unavailable; no conflict decision is shown: {error}"
            )),
            stamp,
        ),
    }
}

fn revocation_reason(label: &str) -> RevocationReason {
    match label {
        "Lost" => RevocationReason::Lost,
        "Stolen" => RevocationReason::Stolen,
        "Compromised" => RevocationReason::Compromised,
        _ => RevocationReason::Retired,
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
        let _ = atomic_file::write_with_mode(&home_peer_health_path(), &bytes, 0o600);
    }
}

fn last_seen_label(last_seen_ms: u64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    let minutes = now_ms.saturating_sub(last_seen_ms) / 60_000;
    if minutes == 0 {
        "Offline · Last seen just now".into()
    } else if minutes < 60 {
        format!("Offline · Last seen {minutes} min ago")
    } else {
        format!("Offline · Last seen {} h ago", minutes / 60)
    }
}

fn peer_health_badge(health: &str) -> &'static str {
    if health.starts_with("Online") && health.contains("Syncing") {
        "Syncing"
    } else if health.starts_with("Online") {
        "Online"
    } else if health == "Checking…" || health == "Connecting…" {
        "Checking"
    } else if health == "Removed" {
        "Removed"
    } else {
        "Offline"
    }
}

pub(crate) fn identity_root() -> PathBuf {
    std::env::var_os("TILO_IDENTITY_ROOT").map_or_else(
        || {
            if PathBuf::from("/var/lib/tilo/device").is_dir() {
                PathBuf::from("/var/lib/tilo")
            } else {
                dirs::data_dir()
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
            Err(tilo_relay::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                // Settings is opened on interactive Tilo devices far more
                // often than on a newly provisioned headless relay. Do not
                // visually opt an unconfigured desktop into server behavior.
                let config = RelayConfig {
                    device_role: DeviceRole::Interactive,
                    services: tilo_relay::ServiceCapabilities::default(),
                    ..RelayConfig::default()
                };
                (config, "Not configured on this device".to_string())
            }
            Err(error) => (
                RelayConfig::default(),
                format!("Configuration could not be read: {error}"),
            ),
        };
        let runtime_status = RelayRuntimeStatus::read(&config, SystemTime::now());
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
            next_status_refresh: Instant::now() + STATUS_REFRESH_INTERVAL,
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
                        self.continuity.record(None, *enabled, unix_seconds());
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
                            .record(Some(capability), *enabled, unix_seconds());
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
                        self.config.workspaces.push(tilo_relay::WorkspaceBinding {
                            id: *id,
                            pin: self.config.device_role.can_serve(),
                            replication: ReplicationIntent::OnDemand,
                            admitted_users: Vec::new(),
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
        let status = RelayRuntimeStatus::read(&self.config, SystemTime::now());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.structure_dirty = true;
        }
        self.refresh_roaming_provenance();
        self.next_status_refresh = Instant::now() + STATUS_REFRESH_INTERVAL;
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
                    .map(|workspace| tilo_relay::WorkspaceBinding {
                        id: workspace.id,
                        pin: true,
                        replication: ReplicationIntent::FullWorkspace,
                        admitted_users: Vec::new(),
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
        self.next_status_refresh = Instant::now();
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the pane composition keeps the four device-mesh policy cards and their explanatory security text in one auditable hierarchy"
    )]
    fn build_composition(&self) -> Box<dyn Widget> {
        let role_state = ui::LocalState::new(role_label(self.config.device_role).to_string());
        let role = Dropdown::from_state(
            ROLE_OPTIONS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
            &role_state,
        )
        .with_id("device_mesh.role")
        .with_placeholder(role_label(self.config.device_role));
        let role = match crate::actions::device_mesh_text_binding("role") {
            Some(binding) => role.on_action(binding),
            None => role.with_enabled(false),
        };

        let mut continuation_rows = vec![
            value_row("Workspace and app activity", "Sync automatically"),
            caption(
                "Pick up your open workspaces, app tabs, reading position, and playback on any device signed in as you.",
            ),
        ];
        if self.config.workspaces.is_empty() {
            continuation_rows.push(value_row("Files", "Download when opened"));
        } else {
            let replication_state =
                ui::LocalState::new(replication_label(&self.common_replication()).to_string());
            let replication = Dropdown::from_state(
                REPLICATION_OPTIONS
                    .iter()
                    .map(|label| (*label).to_string())
                    .collect(),
                &replication_state,
            )
            .with_id("device_mesh.replication")
            .with_placeholder(replication_label(&self.common_replication()));
            let replication = match crate::actions::device_mesh_text_binding("replication") {
                Some(binding) => replication.on_action(binding),
                None => replication.with_enabled(false),
            };
            continuation_rows.push(choice_row_described(
                "Files",
                replication_description(&self.common_replication()),
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
            continuation_rows.push(caption(&self.load_status));
        }

        let mut availability_rows = vec![choice_row_described(
            "This device",
            "Use it normally, sync while it is awake, or make it the always-on home for your workspaces and files.",
            Box::new(role),
        )];
        if shows_advanced_services(self.config.device_role) {
            availability_rows.push(setting_row_described(
                "Home peer",
                match self.runtime_status {
                    RelayRuntimeStatus::Ready => "All selected workspace files are available here.",
                    RelayRuntimeStatus::Online => "This device is reachable by your other devices.",
                    RelayRuntimeStatus::Syncing => {
                        "Keep this device powered and online while file copies finish."
                    }
                    RelayRuntimeStatus::Offline => "The sync service is not currently reachable.",
                },
                semantic_status_pill(
                    self.runtime_status.label(),
                    runtime_status_tone(self.runtime_status),
                ),
            ));
        }
        availability_rows.push(value_row(
            "Workspaces",
            &self.config.workspaces.len().to_string(),
        ));
        availability_rows.push(caption(match self.config.device_role {
            DeviceRole::Interactive => {
                "Choose Always-on server to make this device your always-on sync home."
            }
            DeviceRole::Hybrid => {
                "Workspace updates stay available to your other devices while this device is awake."
            }
            DeviceRole::AlwaysOnServer if self.config.workspaces.is_empty() => {
                "Workspaces added to this device stay available around the clock."
            }
            DeviceRole::AlwaysOnServer
                if self.common_replication() == ReplicationIntent::FullWorkspace =>
            {
                "Open workspaces, app progress, and full file copies stay available from this device."
            }
            DeviceRole::AlwaysOnServer => {
                "Open workspaces and app progress stay available from this device around the clock."
            }
        }));

        let workspace_rows = if self.discovered_workspaces.is_empty() {
            rows(vec![caption(
                "Your workspaces appear here as soon as they are opened.",
            )])
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
                            "Keep this workspace available from this device",
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
            "Pairing…".to_string()
        } else if let Some(candidate) = self
            .pairing_candidate
            .as_ref()
            .filter(|_| self.pairing_target == PairingTarget::HomePeer)
        {
            format!(
                "Approve {} · {}",
                candidate.label, candidate.verification_code
            )
        } else {
            "Pair home server".to_string()
        };
        let approve = Box::new(
            Button::new(
                "device_mesh.approve_enrollment",
                &pair_label,
                Some(APPROVE_ENROLLMENT_KEY),
            )
            .with_enabled(!pairing),
        );
        let personal_label = if pairing && self.pairing_target == PairingTarget::PersonalDevice {
            "Pairing…".to_string()
        } else if self.pairing_target == PairingTarget::PersonalDevice {
            self.pairing_candidate.as_ref().map_or_else(
                || "Add device".to_string(),
                |candidate| {
                    format!(
                        "Approve {} · {}",
                        candidate.label, candidate.verification_code
                    )
                },
            )
        } else {
            "Add device".to_string()
        };
        let approve_personal = Box::new(
            Button::new(
                "device_mesh.approve_personal_device",
                &personal_label,
                Some(APPROVE_PERSONAL_DEVICE_KEY),
            )
            .with_enabled(!pairing),
        );
        let mut personal_rows = vec![caption(
            "On the new device, choose Use my existing Tilo identity. Compare the nearby six-digit code, then approve it here. Your open workspaces and trusted home peers appear automatically.",
        )];
        if self.pairing_target == PairingTarget::PersonalDevice
            && (pairing || self.pairing_candidate.is_some() || !self.load_status.is_empty())
        {
            personal_rows.push(caption(&self.load_status));
        }
        let mut enrolled_device_rows = Vec::new();
        for device in &self.personal_devices {
            let armed = self.personal_remove_armed.contains(&device.key);
            let revoke_key = personal_device_key("revoke", &device.key);
            let reason_key = personal_device_key("reason", &device.key);
            let status = if device.revoked {
                "Revoked"
            } else if device.current {
                "This device"
            } else if device.can_host {
                "Home server"
            } else {
                "Trusted"
            };
            enrolled_device_rows.push(setting_row_described(
                &device.label,
                &format!("Identity fingerprint · {}", device.fingerprint),
                if armed {
                    Box::new(
                        Button::new(&revoke_key, "Revoke access", Some(&revoke_key))
                            .with_enabled(!device.current && !device.revoked),
                    )
                } else if device.current || device.revoked || device.can_host {
                    semantic_status_pill(
                        status,
                        if device.current {
                            StatusTone::Success
                        } else {
                            StatusTone::Neutral
                        },
                    )
                } else {
                    Box::new(Button::new(&revoke_key, "Decommission", Some(&revoke_key)))
                },
            ));
            if armed {
                let cancel_key = personal_device_key("cancel", &device.key);
                enrolled_device_rows.push(choice_row_described(
                    "Why is access ending?",
                    "This reason is signed into your device history and cannot be changed later.",
                    Box::new(
                        Dropdown::new(
                            REVOKE_REASONS
                                .iter()
                                .map(|label| (*label).to_string())
                                .collect(),
                            &reason_key,
                        )
                        .with_id(&reason_key)
                        .with_placeholder("Retired"),
                    ),
                ));
                enrolled_device_rows.push(setting_row_described(
                    "Access ends permanently",
                    "The device stops receiving and authoring new workspace changes. Data already on it is not remotely erased; change account passwords too if it is lost or stolen.",
                    Box::new(Button::new(&cancel_key, "Cancel", Some(&cancel_key))),
                ));
            }
        }
        if let Some(error) = &self.personal_device_error {
            enrolled_device_rows.push(caption(&format!("Device list unavailable: {error}")));
        } else if enrolled_device_rows.is_empty() {
            enrolled_device_rows.push(caption(
                "This device appears here after identity setup completes.",
            ));
        }

        let current_device = self
            .personal_devices
            .iter()
            .find(|device| device.current && !device.revoked);
        let current_target = self
            .personal_devices
            .iter()
            .find(|device| !device.current && !device.revoked);
        let target_name = current_target.map_or("No certified target", |device| &device.label);
        let target_detail = current_target.map_or_else(
            || "Add another personal device to make it an eligible Continuity target.".into(),
            |device| {
                format!(
                    "Certified by your Tilo identity · fingerprint {}. Private keys and pairing credentials never appear here.",
                    device.fingerprint
                )
            },
        );
        let source_detail = current_device.map_or_else(
            || "This source is not yet present in the certified-device fleet.".into(),
            |device| {
                format!(
                    "{} · certified fingerprint {}",
                    device.label, device.fingerprint
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
        let mut privacy_rows = vec![setting_row_described(
            "Allow Continuity from this device",
            "Master device-local kill switch. Turning it off stops every facet immediately without forgetting individual choices.",
            Box::new(continuity_toggle),
        )];
        for capability in Capability::ALL {
            let key = continuity_key(capability);
            let status = if self.continuity.effective(capability) {
                format!("{} Active for {target_name}.", capability.description())
            } else if self.continuity.allowed(capability) {
                format!(
                    "{} Prepared, but stopped by the master switch.",
                    capability.description()
                )
            } else {
                format!("{} Off on this device.", capability.description())
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
            privacy_rows.push(setting_row_described(
                capability.label(),
                &status,
                Box::new(toggle),
            ));
        }
        privacy_rows.push(caption(
            "These hardware and privacy choices stay on this device. They are not copied by settings sync.",
        ));
        if let Some(status) = &self.continuity_status {
            privacy_rows.push(caption(status));
        }

        let mut provenance_rows = vec![
            value_row("Current target", target_name),
            caption(&target_detail),
            value_row("Source", &source_detail),
        ];
        if current_target.is_none() {
            provenance_rows.push(caption(
                "No capability can become active until another non-revoked certified device is available.",
            ));
        }

        let audit_rows = if self.continuity.audit.is_empty() {
            vec![caption(
                "No local Continuity control changes have been recorded yet.",
            )]
        } else {
            self.continuity
                .audit
                .iter()
                .rev()
                .take(6)
                .map(|entry| value_row("Local policy change", &entry.summary()))
                .chain(std::iter::once(caption(
                    "Only the latest 24 control changes stay on this device. Clipboard data, notification text, file names, session credentials, addresses, and input payloads are never logged.",
                )))
                .collect()
        };
        let mut roaming_rows = self.roaming_provenance.as_ref().map_or_else(
            || {
                vec![caption(
                    "No roaming settings decision has been published on this device yet.",
                )]
            },
            |provenance| {
                let summaries = provenance_summaries(provenance);
                if summaries.is_empty() {
                    vec![caption(
                        "No personal settings or saved Wi-Fi profiles are roaming.",
                    )]
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
        roaming_rows.push(caption(
            "Settings conflicts use the newest certified revision, then the device identity as a stable tie-break. Wi-Fi rows contain only profile metadata; passwords and sealed envelopes never enter Settings.",
        ));
        let mut home_server_rows = vec![caption(
            "Choose Pair home server while your new Tilo server is starting. It receives its identity and full-sync setup automatically.",
        )];
        if self.registered_home_peers > 0 {
            home_server_rows.push(value_row(
                "Paired home servers",
                &self.registered_home_peers.to_string(),
            ));
        }
        if pairing || self.load_status.starts_with("Home server") {
            home_server_rows.push(caption(&self.load_status));
        }
        let mut sections = vec![
            card("Continue across devices", rows(continuation_rows)),
            card("Continuity privacy", rows(privacy_rows)),
            card("Current Continuity route", rows(provenance_rows)),
            card("Roaming settings & Wi-Fi", rows(roaming_rows)),
            card("Recent Continuity activity", rows(audit_rows)),
            card_with_action(
                "Add a personal device",
                approve_personal,
                rows(personal_rows),
            ),
            card("Your trusted devices", rows(enrolled_device_rows)),
            card("Device availability", rows(availability_rows)),
            card("Synced workspaces", workspace_rows),
            card_with_action("Home server or NAS", approve, rows(home_server_rows)),
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
            let rename = Box::new(Button::new(&rename_action, "Rename", Some(&rename_action)));
            let armed = self.remove_armed.contains(device);
            let remove = Box::new(Button::new(
                &remove_action,
                if armed {
                    "Revoke & remove"
                } else {
                    "Remove server"
                },
                Some(&remove_action),
            ));
            let health = self
                .home_peer_health
                .get(device)
                .cloned()
                .or_else(|| {
                    self.home_peer_last_seen
                        .get(device)
                        .map(|last_seen| last_seen_label(*last_seen))
                })
                .unwrap_or_else(|| "Checking…".into());
            let mut peer_rows = vec![
                setting_row_described("Name", "Name this home server", Box::new(name)),
                setting_row_described(
                    "Apply name",
                    "Updates the authenticated home server",
                    rename,
                ),
                setting_row_described(
                    "Availability",
                    &health,
                    semantic_status_pill(
                        peer_health_badge(&health),
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
                peer_rows.push(setting_row_described(
                    "Files are not remotely erased",
                    "Revoking stops this server receiving new changes. Make sure another device has any files whose only full copy is here.",
                    Box::new(Button::new(
                        &cancel_action,
                        "Cancel",
                        Some(&cancel_action),
                    )),
                ));
            }
            sections.push(card_with_action(&peer.label, remove, rows(peer_rows)));
        }
        if shows_advanced_services(self.config.device_role) {
            sections.push(card(
                "Device services",
                rows(vec![
                    setting_row_described(
                        "Collaboration and chat relay",
                        "Keep workspace updates and messages moving between devices",
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
                        "Remote-control relay",
                        "Connect remote-control sessions through this device when needed",
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
                        "Serve app backends",
                        "Run compatible app services from this device",
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
                        "Accept distributed jobs",
                        "Run compatible background jobs from this device",
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
        peer: &tilo_relay::HomePeerRegistration,
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
                        let now_ms = SystemTime::now()
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
    #[allow(
        clippy::too_many_lines,
        reason = "one synchronization pass consumes the pane's related role, replication, pairing, device-revocation, and server-management controls atomically"
    )]
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
                    let reason = read_str(state, root, &personal_device_key("reason", &device.key))
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
                if let Some(label) = read_str(state, root, &peer_key("name", &peer.device_node_id))
                    && tilo_relay::valid_home_peer_label(&label)
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
        if Instant::now() >= self.next_status_refresh {
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
        let was_handled = *handled;
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                *child,
                taffy,
            );
        }
        self.commit_context_pending |= !was_handled && *handled;
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

    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "devices-sync",
            "Devices & Sync",
            "Change device mesh settings",
        )
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
            Some(Instant::now() + PROCESS_POLL_INTERVAL)
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

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::roaming_provenance::{SCHEMA_VERSION, SettingResolution, WifiResolution};
    use std::time::Instant;
    use tilo_relay::WorkspaceBinding;

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

    #[test]
    fn continuity_ui_exposes_master_and_every_typed_capability_control() {
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

    #[test]
    fn roaming_conflict_summaries_name_deterministic_winners_without_secrets() {
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

    #[test]
    fn future_provenance_is_refused_instead_of_partially_rendered() {
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

    #[test]
    fn continuity_edits_persist_locally_and_append_redacted_audit_rows() {
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

    #[test]
    fn edits_write_the_real_relay_config_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut config = RelayConfig::default();
        config.workspaces.push(WorkspaceBinding {
            id: 7,
            pin: true,
            replication: ReplicationIntent::OnDemand,
            admitted_users: Vec::new(),
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

    #[test]
    fn missing_config_is_honest_and_not_created_until_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut pane = DeviceMeshPane::with_path(&path);
        assert!(pane.load_status.contains("Not configured"));
        assert_eq!(pane.config.device_role, DeviceRole::Interactive);
        assert_eq!(
            pane.config.services,
            tilo_relay::ServiceCapabilities::default()
        );
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(!path.exists());
        pane.apply_command(&crate::actions::DeviceMeshOperation::SetRole {
            role: ROLE_ACTIVE.into(),
        });
        assert!(path.exists());
    }

    #[test]
    fn file_policy_labels_map_to_the_existing_replication_intents() {
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
            replication_description(&ReplicationIntent::OnDemand).contains("preview thumbnails")
        );
        assert!(
            replication_description(&ReplicationIntent::FullWorkspace).contains("full file copies")
        );
    }

    #[test]
    fn advanced_services_only_appear_for_devices_that_can_serve() {
        assert!(!shows_advanced_services(DeviceRole::Interactive));
        assert!(shows_advanced_services(DeviceRole::Hybrid));
        assert!(shows_advanced_services(DeviceRole::AlwaysOnServer));
    }

    #[test]
    fn changing_between_personal_and_serving_roles_requests_a_rebuild() {
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

    #[test]
    fn always_available_is_one_outcome_that_enables_full_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut config = RelayConfig {
            device_role: DeviceRole::Interactive,
            services: tilo_relay::ServiceCapabilities::default(),
            ..RelayConfig::default()
        };
        config.workspaces.push(WorkspaceBinding {
            id: 7,
            pin: false,
            replication: ReplicationIntent::OnDemand,
            admitted_users: Vec::new(),
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

    #[test]
    fn replicated_document_edits_cannot_change_local_device_role() {
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

    #[test]
    fn peer_health_is_a_compact_visual_state_with_detail_beside_it() {
        assert_eq!(peer_health_badge("Online · Files ready"), "Online");
        assert_eq!(peer_health_badge("Online · Syncing files"), "Syncing");
        assert_eq!(peer_health_badge("Offline · Last seen 4 h ago"), "Offline");
        assert_eq!(peer_health_badge("Checking…"), "Checking");
    }

    #[test]
    fn home_server_removal_requires_review_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
        pane.home_peers = vec![tilo_relay::HomePeerRegistration {
            schema_version: tilo_relay::HomePeerRegistration::CURRENT_SCHEMA,
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

    #[test]
    fn personal_device_decommission_is_armed_before_any_revocation() {
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
            read_str(&doc, &automerge::ROOT, &personal_device_key("reason", &key)).as_deref(),
            Some("Retired")
        );
    }

    #[test]
    fn discovered_workspace_toggle_updates_the_real_binding_set() {
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

    #[test]
    fn active_config_apply_is_polled_without_blocking_and_coalesces_edits() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
        pane.config_path = PathBuf::from(DEFAULT_CONFIG);
        let mut command = Command::new("sleep");
        command.arg("2");
        pane.apply_child = Some(
            bounded_command::spawn_prepared(command, None, 64, Duration::from_secs(5)).unwrap(),
        );

        let started = Instant::now();
        pane.persist();
        pane.poll_background_tasks();

        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(pane.apply_child.is_some());
        assert!(pane.apply_queued);
        pane.apply_child = None;
    }

    #[test]
    fn pairing_process_transitions_to_success_and_allows_another_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
        pane.pairing_child = Some(
            bounded_command::spawn_prepared(Command::new("true"), None, 64, Duration::from_secs(1))
                .unwrap(),
        );
        for _ in 0..100 {
            pane.poll_background_tasks();
            if pane.pairing_child.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(pane.pairing_child.is_none());
        assert_eq!(pane.load_status, "Home server paired");
    }

    #[test]
    fn duplicate_pair_action_is_ignored_while_pairing_is_active() {
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

    #[test]
    fn fresh_daemon_metrics_drive_online_status() {
        let dir = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: dir.path().to_path_buf(),
            device_role: DeviceRole::Hybrid,
            ..RelayConfig::default()
        };
        let metrics = tilo_relay::metrics::MetricsSnapshot {
            deltas_received: 42,
            ..Default::default()
        };
        tilo_relay::store::save_metrics(&config.state_root, &metrics).unwrap();
        let modified = std::fs::metadata(config.state_root.join(tilo_relay::store::METRICS_FILE))
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

    #[test]
    fn always_on_status_requires_fresh_complete_file_residency() {
        let dir = tempfile::tempdir().unwrap();
        let proof = dir.path().join("full-replica.json");
        let config = RelayConfig {
            state_root: dir.path().to_path_buf(),
            full_replica_status: proof.clone(),
            device_role: DeviceRole::AlwaysOnServer,
            services: tilo_relay::ServiceCapabilities::personal_server(),
            ..RelayConfig::default()
        };
        tilo_relay::store::save_metrics(
            &config.state_root,
            &tilo_relay::metrics::MetricsSnapshot::default(),
        )
        .unwrap();
        let now = SystemTime::now();
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
