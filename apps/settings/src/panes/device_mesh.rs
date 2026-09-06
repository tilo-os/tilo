//! Devices & Sync — configure seamless continuation and always-on sync.
//!
//! The pane reads and writes the same `RelayConfig` consumed by `qos-relay`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use qos_relay::{DeviceRole, RelayConfig, ReplicationIntent};
use qos_user_identity::RevocationReason;
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action, set_text_action};
use crate::widgets::{
    caption, card, card_with_action, cards, choice_row_described, rows, setting_row_described,
    status_pill, value_row,
};

const DEFAULT_CONFIG: &str = "/etc/qos/relay.toml";
const ROLE_KEY: &str = "device_mesh.role";
const REPLICATION_KEY: &str = "device_mesh.replication";
const INBOUND_RELAY_KEY: &str = "device_mesh.inbound_relay";
const REMOTE_RELAY_KEY: &str = "device_mesh.remote_control_relay";
const APP_SERVING_KEY: &str = "device_mesh.app_serving";
const COMPUTE_KEY: &str = "device_mesh.compute";
const APPROVE_ENROLLMENT_KEY: &str = "device_mesh.approve_enrollment.edit";
const APPROVE_PERSONAL_DEVICE_KEY: &str = "device_mesh.approve_personal_device.edit";
const CONFIRM_ROLE_KEY: &str = "device_mesh.confirm_role.edit";
const CANCEL_ROLE_KEY: &str = "device_mesh.cancel_role.edit";
const CONTROL_HELPER: &str = "/usr/libexec/qos-home-peer-control";
const RELAY_ADMIN: &str = "/usr/bin/qos-relay-admin";

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
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

fn draft_key(key: &str) -> String {
    format!("{key}.edit")
}

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
        let metrics_path = config.state_root.join(qos_relay::store::METRICS_FILE);
        let fresh = std::fs::metadata(metrics_path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified).unwrap_or(Duration::ZERO) <= STATUS_FRESHNESS
            });
        if !fresh {
            return Self::Offline;
        }
        match qos_relay::store::load_metrics(&config.state_root) {
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

fn runtime_status_tint(status: RelayRuntimeStatus) -> vello::peniko::Color {
    let theme = Theme::dark();
    match status {
        RelayRuntimeStatus::Ready | RelayRuntimeStatus::Online => theme.status_success,
        RelayRuntimeStatus::Syncing => theme.tokens.colors.semantic.warning,
        RelayRuntimeStatus::Offline => theme.text_dim,
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
    home_peers: Vec<qos_relay::HomePeerRegistration>,
    home_peer_health: HashMap<String, String>,
    home_peer_last_seen: HashMap<String, u64>,
    personal_devices: Vec<crate::device_fleet::DeviceView>,
    personal_device_error: Option<String>,
    personal_remove_armed: std::collections::HashSet<[u8; 32]>,
    management_child: Option<Child>,
    management_device: Option<String>,
    next_peer_health: usize,
    remove_armed: std::collections::HashSet<String>,
    pending_role: Option<DeviceRole>,
    apply_child: Option<Child>,
    apply_queued: bool,
    pairing_child: Option<Child>,
    pairing_discovery: bool,
    pairing_candidate: Option<PairCandidate>,
    pairing_target: PairingTarget,
    next_status_refresh: Instant,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    actions: ui::ActionHost,
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
        |dir| dir.join("qos/state.json"),
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
                        .and_then(|room| qos_relay::room_workspace_id(room).ok())
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

fn registered_home_peer_count() -> usize {
    registered_home_peers().len()
}

fn registered_home_peers() -> Vec<qos_relay::HomePeerRegistration> {
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
    std::env::var_os("QOS_IDENTITY_ROOT").map_or_else(
        || {
            if PathBuf::from("/var/lib/qos/device").is_dir() {
                PathBuf::from("/var/lib/qos")
            } else {
                dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from(".local/share"))
                    .join("qos")
            }
        },
        PathBuf::from,
    )
}

fn poll_child(child: &mut Child) -> std::io::Result<Option<ExitStatus>> {
    child.try_wait()
}

fn take_reaped_child(slot: &mut Option<Child>) -> Option<Child> {
    let mut child = slot.take()?;
    let _ = child.wait();
    Some(child)
}

fn child_error(child: &mut Child, fallback: &str) -> String {
    let mut detail = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut detail);
    }
    let detail = detail.trim();
    if detail.is_empty() {
        fallback.to_owned()
    } else {
        detail.to_owned()
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
            Err(qos_relay::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                // Settings is opened on interactive qOS devices far more
                // often than on a newly provisioned headless relay. Do not
                // visually opt an unconfigured desktop into server behavior.
                let config = RelayConfig {
                    device_role: DeviceRole::Interactive,
                    services: qos_relay::ServiceCapabilities::default(),
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
            personal_remove_armed: std::collections::HashSet::new(),
            management_child: None,
            management_device: None,
            next_peer_health: 0,
            remove_armed: std::collections::HashSet::new(),
            pending_role: None,
            apply_child: None,
            apply_queued: false,
            pairing_child: None,
            pairing_discovery: false,
            pairing_candidate: None,
            pairing_target: PairingTarget::HomePeer,
            next_status_refresh: Instant::now() + STATUS_REFRESH_INTERVAL,
            composed: None,
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
        }
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        Self::from_path(path.to_path_buf())
    }

    fn common_replication(&self) -> ReplicationIntent {
        self.config
            .workspaces
            .first()
            .map_or(ReplicationIntent::OnDemand, |workspace| {
                workspace.replication.clone()
            })
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Settings", "Settings", action),
        );
    }

    fn refresh_runtime_status(&mut self) {
        let status = RelayRuntimeStatus::read(&self.config, SystemTime::now());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.structure_dirty = true;
        }
        self.next_status_refresh = Instant::now() + STATUS_REFRESH_INTERVAL;
    }

    fn apply_role_change(
        &mut self,
        role: DeviceRole,
        state: &mut AutoCommit,
        _root: &ObjId,
    ) -> bool {
        if role == self.config.device_role {
            return false;
        }
        if shows_advanced_services(role) != shows_advanced_services(self.config.device_role) {
            self.structure_dirty = true;
        }
        self.config.device_role = role;
        self.config.services.workspace_replication = role.can_serve();
        self.config.services.inbound_relay = role.can_serve();
        self.dispatch(state, &set_bool_action(INBOUND_RELAY_KEY, role.can_serve()));
        for workspace in &mut self.config.workspaces {
            workspace.pin = role.can_serve();
        }
        if role == DeviceRole::AlwaysOnServer {
            if self.config.workspaces.is_empty() {
                self.config.workspaces = self
                    .discovered_workspaces
                    .iter()
                    .map(|workspace| qos_relay::WorkspaceBinding {
                        id: workspace.id,
                        pin: true,
                        replication: ReplicationIntent::FullWorkspace,
                        admitted_users: Vec::new(),
                    })
                    .collect();
                for workspace in self.discovered_workspaces.clone() {
                    self.dispatch(state, &set_bool_action(workspace_key(workspace.id), true));
                }
            }
            self.config.services.full_content_replication = true;
            for workspace in &mut self.config.workspaces {
                workspace.replication = ReplicationIntent::FullWorkspace;
            }
            self.dispatch(
                state,
                &set_text_action(
                    REPLICATION_KEY,
                    replication_label(&ReplicationIntent::FullWorkspace),
                ),
            );
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
        let role = Dropdown::new(
            ROLE_OPTIONS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
            &draft_key(ROLE_KEY),
        )
        .with_id("device_mesh.role")
        .with_placeholder(role_label(self.config.device_role));

        let mut continuation_rows = vec![
            value_row("Workspace and app activity", "Sync automatically"),
            caption(
                "Pick up your open workspaces, app tabs, reading position, and playback on any device signed in as you.",
            ),
        ];
        if self.config.workspaces.is_empty() {
            continuation_rows.push(value_row("Files", "Download when opened"));
        } else {
            let replication = Dropdown::new(
                REPLICATION_OPTIONS
                    .iter()
                    .map(|label| (*label).to_string())
                    .collect(),
                &draft_key(REPLICATION_KEY),
            )
            .with_id("device_mesh.replication")
            .with_placeholder(replication_label(&self.common_replication()));
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
                status_pill(
                    self.runtime_status.label(),
                    runtime_status_tint(self.runtime_status),
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
                        setting_row_described(
                            &workspace.name,
                            "Keep this workspace available from this device",
                            Box::new(Toggle::new(&key, &draft_key(&key)).with_id(&key)),
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
            "On the new device, choose Use my existing qOS identity. Compare the nearby six-digit code, then approve it here. Your open workspaces and trusted home peers appear automatically.",
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
                    status_pill(
                        status,
                        if device.current {
                            Theme::dark().status_success
                        } else {
                            Theme::dark().text_dim
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
        let mut home_server_rows = vec![caption(
            "Choose Pair home server while your new qOS server is starting. It receives its identity and full-sync setup automatically.",
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
        if let Some(pending) = self.pending_role {
            let making_server = pending == DeviceRole::AlwaysOnServer;
            let confirm = Box::new(Button::new(
                "device_mesh.confirm_role",
                if making_server {
                    "Make always-on"
                } else {
                    "Stop always-on"
                },
                Some(CONFIRM_ROLE_KEY),
            ));
            let cancel = Box::new(Button::new(
                "device_mesh.cancel_role",
                if making_server {
                    "Keep current setup"
                } else {
                    "Keep always-on"
                },
                Some(CANCEL_ROLE_KEY),
            ));
            sections.insert(
                4,
                card_with_action(
                    if making_server {
                        "Make this device always-on?"
                    } else {
                        "Stop hosting from this device?"
                    },
                    confirm,
                    rows(vec![
                        caption(if making_server {
                            "Selected workspaces and their files will be copied to this device. It will relay collaboration and chat while powered and online, using additional storage, network, and energy."
                        } else {
                            "Other devices will no longer be able to rely on this one while it sleeps or is powered off. Existing local files stay on this device and continue to follow the file policy below."
                        }),
                        setting_row_described(
                            "This affects availability",
                            "It does not change collaborators or their workspace permissions.",
                            cancel,
                        ),
                    ]),
                ),
            );
        }
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
                    status_pill(
                        peer_health_badge(&health),
                        if health.starts_with("Online") {
                            Theme::dark().status_success
                        } else if health.contains("Syncing") || health == "Checking…" {
                            Theme::dark().tokens.colors.semantic.warning
                        } else {
                            Theme::dark().text_dim
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
                "Advanced",
                rows(vec![
                    setting_row_described(
                        "Collaboration and chat relay",
                        "Keep workspace updates and messages moving between devices",
                        Box::new(
                            Toggle::new(
                                "settings.device_mesh.inbound_relay",
                                &draft_key(INBOUND_RELAY_KEY),
                            )
                            .with_id("device_mesh.inbound_relay"),
                        ),
                    ),
                    setting_row_described(
                        "Remote-control relay",
                        "Connect remote-control sessions through this device when needed",
                        Box::new(
                            Toggle::new(
                                "settings.device_mesh.remote_relay",
                                &draft_key(REMOTE_RELAY_KEY),
                            )
                            .with_id("device_mesh.remote_relay"),
                        ),
                    ),
                    setting_row_described(
                        "Serve app backends",
                        "Run compatible app services from this device",
                        Box::new(
                            Toggle::new(
                                "settings.device_mesh.app_serving",
                                &draft_key(APP_SERVING_KEY),
                            )
                            .with_id("device_mesh.app_serving"),
                        ),
                    ),
                    setting_row_described(
                        "Accept distributed jobs",
                        "Run compatible background jobs from this device",
                        Box::new(
                            Toggle::new("settings.device_mesh.compute", &draft_key(COMPUTE_KEY))
                                .with_id("device_mesh.compute"),
                        ),
                    ),
                ]),
            ));
        }

        cards(sections)
    }

    fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                automerge::ScalarValue::Boolean(value) => Some(*value),
                _ => None,
            },
            _ => None,
        }
    }

    fn apply_bool(state: &AutoCommit, root: &ObjId, key: &str, current: &mut bool) -> bool {
        if let Some(value) = Self::read_bool(state, root, key)
            && value != *current
        {
            *current = value;
            return true;
        }
        false
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

    fn spawn_apply(&self) -> Result<Child, String> {
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
        let mut child = Command::new("pkexec")
            .args([CONTROL_HELPER, "apply"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start system setup: {error}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "system setup input was unavailable".to_string())?
            .write_all(request.to_string().as_bytes())
            .map_err(|error| format!("could not send system setup: {error}"))?;
        Ok(child)
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
            self.load_status = match Command::new("/usr/bin/qos-enroll")
                .args([discover_command, "--timeout", "15"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
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
        self.load_status = match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
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
        peer: &qos_relay::HomePeerRegistration,
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
        match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
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
            if let Ok(child) = Command::new(RELAY_ADMIN)
                .arg("home-peer-retry-removals")
                .arg("--identity-root")
                .arg(identity_root())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
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
        let apply_result = self.apply_child.as_mut().map(poll_child);
        match apply_result {
            Some(Ok(Some(status))) => {
                let Some(mut child) = take_reaped_child(&mut self.apply_child) else {
                    self.load_status = "Not saved: setup process disappeared".into();
                    self.structure_dirty = true;
                    return;
                };
                if status.success() {
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
                        child_error(&mut child, "administrator approval was not granted")
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
            Some(Ok(None)) | None => {}
        }

        let pairing_result = self.pairing_child.as_mut().map(poll_child);
        match pairing_result {
            Some(Ok(Some(status))) => {
                let Some(mut child) = take_reaped_child(&mut self.pairing_child) else {
                    self.load_status = "Pairing stopped: setup process disappeared".into();
                    self.structure_dirty = true;
                    return;
                };
                if status.success() && self.pairing_discovery {
                    let mut output = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        let _ = stdout.read_to_string(&mut output);
                    }
                    match serde_json::from_str::<PairCandidate>(output.trim()) {
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
                } else if status.success() {
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
                        child_error(&mut child, "the pairing window closed or was cancelled")
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
            Some(Ok(None)) | None => {}
        }

        let management_result = self.management_child.as_mut().map(poll_child);
        match management_result {
            Some(Ok(Some(status))) => {
                let Some(mut child) = take_reaped_child(&mut self.management_child) else {
                    self.load_status = "Home-server check stopped unexpectedly".into();
                    self.structure_dirty = true;
                    return;
                };
                let device = self.management_device.take().unwrap_or_default();
                if status.success() {
                    let mut output = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        let _ = stdout.read_to_string(&mut output);
                    }
                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(output.trim()) {
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
                    let detail = child_error(&mut child, "not reachable");
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
            Some(Ok(None)) | None => {}
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
        if self.needs_seed {
            let _ = state.put(root, ROLE_KEY, role_label(self.config.device_role));
            let _ = state.put(
                root,
                draft_key(ROLE_KEY),
                role_label(self.config.device_role),
            );
            let _ = state.put(
                root,
                REPLICATION_KEY,
                replication_label(&self.common_replication()),
            );
            let _ = state.put(
                root,
                draft_key(REPLICATION_KEY),
                replication_label(&self.common_replication()),
            );
            for (key, value) in [
                (INBOUND_RELAY_KEY, self.config.services.inbound_relay),
                (REMOTE_RELAY_KEY, self.config.services.remote_control_relay),
                (APP_SERVING_KEY, self.config.services.app_serving),
                (COMPUTE_KEY, self.config.services.compute),
            ] {
                let _ = state.put(root, key, value);
                let _ = state.put(root, draft_key(key), value);
            }
            for workspace in &self.discovered_workspaces {
                let selected = self
                    .config
                    .workspaces
                    .iter()
                    .any(|binding| binding.id == workspace.id);
                let _ = state.put(root, workspace_key(workspace.id), selected);
                let _ = state.put(root, draft_key(&workspace_key(workspace.id)), selected);
            }
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
            let _ = state.put(root, CONFIRM_ROLE_KEY, false);
            let _ = state.put(root, CANCEL_ROLE_KEY, false);
            self.needs_seed = false;
        }

        // Durable controls edit draft keys only.  Promote a changed draft by
        // the same SettingsAction executor used by palette actions; local
        // command latches below intentionally remain `.edit` presentation
        // state and are never replayed as preferences.
        let role = role_label(self.config.device_role);
        if Self::read_str(state, root, ROLE_KEY).as_deref() == Some(role)
            && let Some(draft) = Self::read_str(state, root, &draft_key(ROLE_KEY))
            && draft != role
            && role_from_label(&draft).is_some()
        {
            self.dispatch(state, &set_text_action(ROLE_KEY, draft));
        }
        let replication = replication_label(&self.common_replication());
        if Self::read_str(state, root, REPLICATION_KEY).as_deref() == Some(replication)
            && let Some(draft) = Self::read_str(state, root, &draft_key(REPLICATION_KEY))
            && draft != replication
            && replication_from_label(&draft).is_some()
        {
            self.dispatch(state, &set_text_action(REPLICATION_KEY, draft));
        }
        for (key, current) in [
            (INBOUND_RELAY_KEY, self.config.services.inbound_relay),
            (REMOTE_RELAY_KEY, self.config.services.remote_control_relay),
            (APP_SERVING_KEY, self.config.services.app_serving),
            (COMPUTE_KEY, self.config.services.compute),
        ] {
            if Self::read_bool(state, root, key) == Some(current)
                && let Some(draft) = Self::read_bool(state, root, &draft_key(key))
                && draft != current
            {
                self.dispatch(state, &set_bool_action(key, draft));
            }
        }
        for workspace in self.discovered_workspaces.clone() {
            let key = workspace_key(workspace.id);
            let selected = self
                .config
                .workspaces
                .iter()
                .any(|binding| binding.id == workspace.id);
            if Self::read_bool(state, root, &key) == Some(selected)
                && let Some(draft) = Self::read_bool(state, root, &draft_key(&key))
                && draft != selected
            {
                self.dispatch(state, &set_bool_action(key, draft));
            }
        }

        let mut changed = false;
        if Self::read_bool(state, root, CANCEL_ROLE_KEY) == Some(true) {
            let _ = state.put(root, CANCEL_ROLE_KEY, false);
            self.pending_role = None;
            self.structure_dirty = true;
        }
        if Self::read_bool(state, root, CONFIRM_ROLE_KEY) == Some(true) {
            let _ = state.put(root, CONFIRM_ROLE_KEY, false);
            if let Some(role) = self.pending_role.take() {
                changed |= self.apply_role_change(role, state, root);
                self.dispatch(state, &set_text_action(ROLE_KEY, role_label(role)));
                self.structure_dirty = true;
            }
        }
        if let Some(role) = Self::read_str(state, root, ROLE_KEY)
            .as_deref()
            .and_then(role_from_label)
            && role != self.config.device_role
        {
            if role == DeviceRole::AlwaysOnServer
                || self.config.device_role == DeviceRole::AlwaysOnServer
            {
                self.pending_role = Some(role);
                self.dispatch(
                    state,
                    &set_text_action(ROLE_KEY, role_label(self.config.device_role)),
                );
                self.structure_dirty = true;
            } else {
                changed |= self.apply_role_change(role, state, root);
            }
        }
        if let Some(intent) = Self::read_str(state, root, REPLICATION_KEY)
            .as_deref()
            .and_then(replication_from_label)
            && self
                .config
                .workspaces
                .iter()
                .any(|workspace| workspace.replication != intent)
        {
            for workspace in &mut self.config.workspaces {
                workspace.replication = intent.clone();
            }
            self.config.services.full_content_replication =
                intent == ReplicationIntent::FullWorkspace;
            changed = true;
        }
        changed |= Self::apply_bool(
            state,
            root,
            INBOUND_RELAY_KEY,
            &mut self.config.services.inbound_relay,
        );
        changed |= Self::apply_bool(
            state,
            root,
            REMOTE_RELAY_KEY,
            &mut self.config.services.remote_control_relay,
        );
        changed |= Self::apply_bool(
            state,
            root,
            APP_SERVING_KEY,
            &mut self.config.services.app_serving,
        );
        changed |= Self::apply_bool(state, root, COMPUTE_KEY, &mut self.config.services.compute);
        for workspace in self.discovered_workspaces.clone() {
            let selected =
                Self::read_bool(state, root, &workspace_key(workspace.id)).unwrap_or(false);
            if selected
                && !self
                    .config
                    .workspaces
                    .iter()
                    .any(|binding| binding.id == workspace.id)
            {
                self.config.workspaces.push(qos_relay::WorkspaceBinding {
                    id: workspace.id,
                    pin: self.config.device_role.can_serve(),
                    replication: if self.config.device_role == DeviceRole::AlwaysOnServer {
                        ReplicationIntent::FullWorkspace
                    } else {
                        ReplicationIntent::OnDemand
                    },
                    admitted_users: Vec::new(),
                });
                changed = true;
            } else if !selected && self.config.remove_workspace(workspace.id) {
                changed = true;
            }
        }
        if Self::read_bool(state, root, APPROVE_ENROLLMENT_KEY) == Some(true) {
            let _ = state.put(root, APPROVE_ENROLLMENT_KEY, false);
            if self.pairing_child.is_none() {
                self.approve_enrollment(PairingTarget::HomePeer);
            }
        }
        if Self::read_bool(state, root, APPROVE_PERSONAL_DEVICE_KEY) == Some(true) {
            let _ = state.put(root, APPROVE_PERSONAL_DEVICE_KEY, false);
            if self.pairing_child.is_none() {
                self.approve_enrollment(PairingTarget::PersonalDevice);
            }
        }
        for device in self.personal_devices.clone() {
            let revoke_key = personal_device_key("revoke", &device.key);
            let cancel_key = personal_device_key("cancel", &device.key);
            if Self::read_bool(state, root, &cancel_key) == Some(true) {
                let _ = state.put(root, &cancel_key, false);
                self.personal_remove_armed.remove(&device.key);
                self.structure_dirty = true;
            }
            if Self::read_bool(state, root, &revoke_key) == Some(true) {
                let _ = state.put(root, &revoke_key, false);
                if self.personal_remove_armed.remove(&device.key) {
                    let reason =
                        Self::read_str(state, root, &personal_device_key("reason", &device.key))
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
            if Self::read_bool(state, root, &rename_key) == Some(true) {
                let _ = state.put(root, &rename_key, false);
                if let Some(label) =
                    Self::read_str(state, root, &peer_key("name", &peer.device_node_id))
                    && qos_relay::valid_home_peer_label(&label)
                {
                    self.start_home_peer_command(&peer, "home-peer-rename", Some(label.trim()));
                }
            }
            let remove_key = peer_key("remove", &peer.device_node_id);
            let cancel_remove_key = peer_key("cancel_remove", &peer.device_node_id);
            if Self::read_bool(state, root, &cancel_remove_key) == Some(true) {
                let _ = state.put(root, &cancel_remove_key, false);
                self.remove_armed.remove(&peer.device_node_id);
                self.structure_dirty = true;
            }
            if Self::read_bool(state, root, &remove_key) == Some(true) {
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
        if changed {
            self.persist();
            self.structure_dirty = true;
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

    fn take_commit_context(&mut self) -> Option<ui::qos_actions::ActionContext> {
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

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
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
    use qos_relay::WorkspaceBinding;
    use std::time::Instant;

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
        doc.put(&automerge::ROOT, draft_key(ROLE_KEY), ROLE_ACTIVE)
            .unwrap();
        doc.put(
            &automerge::ROOT,
            draft_key(REPLICATION_KEY),
            REPLICATION_FULL,
        )
        .unwrap();
        doc.put(&automerge::ROOT, draft_key(REMOTE_RELAY_KEY), false)
            .unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(pane.pending_role, Some(DeviceRole::Hybrid));
        doc.put(&automerge::ROOT, CONFIRM_ROLE_KEY, true).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

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
            qos_relay::ServiceCapabilities::default()
        );
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(!path.exists());
        doc.put(&automerge::ROOT, ROLE_KEY, ROLE_ACTIVE).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
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

        doc.put(&automerge::ROOT, ROLE_KEY, ROLE_ACTIVE).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

        assert!(pane.take_structure_dirty());
        assert_eq!(pane.config.device_role, DeviceRole::Hybrid);
    }

    #[test]
    fn always_available_is_one_outcome_that_enables_full_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let mut config = RelayConfig {
            device_role: DeviceRole::Interactive,
            services: qos_relay::ServiceCapabilities::default(),
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
        doc.put(&automerge::ROOT, ROLE_KEY, ROLE_SERVER).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

        assert_eq!(pane.pending_role, Some(DeviceRole::AlwaysOnServer));
        assert_eq!(pane.config.device_role, DeviceRole::Interactive);
        assert_eq!(
            RelayConfig::load(&path).unwrap().device_role,
            DeviceRole::Interactive,
            "choosing the high-impact role must not apply before review"
        );

        doc.put(&automerge::ROOT, CONFIRM_ROLE_KEY, true).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

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
    fn cancelling_server_promotion_preserves_the_existing_setup() {
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

        doc.put(&automerge::ROOT, ROLE_KEY, ROLE_SERVER).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
        doc.put(&automerge::ROOT, CANCEL_ROLE_KEY, true).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);

        assert_eq!(pane.pending_role, None);
        assert_eq!(pane.config.device_role, DeviceRole::Interactive);
        assert_eq!(
            DeviceMeshPane::read_str(&doc, &automerge::ROOT, ROLE_KEY).as_deref(),
            Some(ROLE_PERSONAL)
        );
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
        pane.home_peers = vec![qos_relay::HomePeerRegistration {
            schema_version: qos_relay::HomePeerRegistration::CURRENT_SCHEMA,
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
            DeviceMeshPane::read_str(&doc, &automerge::ROOT, &personal_device_key("reason", &key))
                .as_deref(),
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
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        doc.put(&automerge::ROOT, workspace_key(44), true).unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
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
        pane.apply_child = Some(Command::new("sleep").arg("2").spawn().unwrap());

        let started = Instant::now();
        pane.persist();
        pane.poll_background_tasks();

        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(pane.apply_child.is_some());
        assert!(pane.apply_queued);
        let mut child = pane.apply_child.take().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn pairing_process_transitions_to_success_and_allows_another_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
        pane.pairing_child = Some(Command::new("true").spawn().unwrap());
        pane.pairing_child.as_mut().unwrap().wait().unwrap();
        pane.poll_background_tasks();
        assert!(pane.pairing_child.is_none());
        assert_eq!(pane.load_status, "Home server paired");
    }

    #[test]
    fn duplicate_pair_action_is_ignored_while_pairing_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshPane::with_path(&dir.path().join("relay.toml"));
        pane.pairing_child = Some(Command::new("sleep").arg("2").spawn().unwrap());
        let child_id = pane.pairing_child.as_ref().unwrap().id();
        pane.approve_enrollment(PairingTarget::HomePeer);
        assert_eq!(pane.pairing_child.as_ref().unwrap().id(), child_id);
        let mut child = pane.pairing_child.take().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn fresh_daemon_metrics_drive_online_status() {
        let dir = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: dir.path().to_path_buf(),
            device_role: DeviceRole::Hybrid,
            ..RelayConfig::default()
        };
        let metrics = qos_relay::metrics::MetricsSnapshot {
            deltas_received: 42,
            ..Default::default()
        };
        qos_relay::store::save_metrics(&config.state_root, &metrics).unwrap();
        let modified = std::fs::metadata(config.state_root.join(qos_relay::store::METRICS_FILE))
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
            services: qos_relay::ServiceCapabilities::personal_server(),
            ..RelayConfig::default()
        };
        qos_relay::store::save_metrics(
            &config.state_root,
            &qos_relay::metrics::MetricsSnapshot::default(),
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
