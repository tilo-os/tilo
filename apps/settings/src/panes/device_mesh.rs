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
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{
    caption, card, card_with_action, cards, choice_row_described, rows, setting_row_described,
    value_row,
};

const DEFAULT_CONFIG: &str = "/etc/qos/relay.toml";
const ROLE_KEY: &str = "device_mesh.role";
const REPLICATION_KEY: &str = "device_mesh.replication";
const INBOUND_RELAY_KEY: &str = "device_mesh.inbound_relay";
const REMOTE_RELAY_KEY: &str = "device_mesh.remote_control_relay";
const APP_SERVING_KEY: &str = "device_mesh.app_serving";
const COMPUTE_KEY: &str = "device_mesh.compute";
const APPROVE_ENROLLMENT_KEY: &str = "device_mesh.approve_enrollment";
const CONTROL_HELPER: &str = "/usr/libexec/qos-home-peer-control";

const ROLE_OPTIONS: &[&str] = &[
    "Use normally",
    "Sync while awake",
    "Keep everything available",
];
const REPLICATION_OPTIONS: &[&str] = &[
    "Continue without files",
    "Download files when opened",
    "Keep all files synced",
];
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn role_label(role: DeviceRole) -> &'static str {
    match role {
        DeviceRole::Interactive => ROLE_OPTIONS[0],
        DeviceRole::Hybrid => ROLE_OPTIONS[1],
        DeviceRole::AlwaysOnServer => ROLE_OPTIONS[2],
    }
}

fn role_from_label(label: &str) -> Option<DeviceRole> {
    match label {
        "Use normally" => Some(DeviceRole::Interactive),
        "Sync while awake" => Some(DeviceRole::Hybrid),
        "Keep everything available" => Some(DeviceRole::AlwaysOnServer),
        _ => None,
    }
}

fn replication_label(intent: &ReplicationIntent) -> &'static str {
    match intent {
        ReplicationIntent::WorkspaceOnly => REPLICATION_OPTIONS[0],
        ReplicationIntent::FullWorkspace => REPLICATION_OPTIONS[2],
        ReplicationIntent::OnDemand | ReplicationIntent::Resources(_) => REPLICATION_OPTIONS[1],
    }
}

fn replication_from_label(label: &str) -> Option<ReplicationIntent> {
    match label {
        "Continue without files" => Some(ReplicationIntent::WorkspaceOnly),
        "Download files when opened" => Some(ReplicationIntent::OnDemand),
        "Keep all files synced" => Some(ReplicationIntent::FullWorkspace),
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
            .map(|modified| {
                now.duration_since(modified).unwrap_or(Duration::ZERO) <= STATUS_FRESHNESS
            })
            .unwrap_or(false);
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

/// Editor for the daemon's real device-mesh configuration.
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
    apply_child: Option<Child>,
    apply_queued: bool,
    pairing_child: Option<Child>,
    next_status_refresh: Instant,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredWorkspace {
    id: u64,
    name: String,
}

fn workspace_key(id: u64) -> String {
    format!("device_mesh.workspace.{id}")
}

fn discover_workspaces(config: &RelayConfig) -> Vec<DiscoveredWorkspace> {
    let path = dirs::config_dir()
        .map(|dir| dir.join("qos/state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"));
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
    std::fs::read(identity_root().join("home-peers.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
        .map_or(0, |peers| peers.len())
}

fn identity_root() -> PathBuf {
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
                let mut config = RelayConfig::default();
                // Settings is opened on interactive qOS devices far more
                // often than on a newly provisioned headless relay. Do not
                // visually opt an unconfigured desktop into server behavior.
                config.device_role = DeviceRole::Interactive;
                config.services = Default::default();
                (config, "Not configured on this device".to_string())
            }
            Err(error) => (
                RelayConfig::default(),
                format!("Configuration could not be read: {error}"),
            ),
        };
        let runtime_status = RelayRuntimeStatus::read(&config, SystemTime::now());
        let discovered_workspaces = discover_workspaces(&config);
        Self {
            config_path,
            config,
            load_status,
            needs_seed: true,
            commit_context_pending: false,
            structure_dirty: false,
            runtime_status,
            discovered_workspaces,
            registered_home_peers: registered_home_peer_count(),
            apply_child: None,
            apply_queued: false,
            pairing_child: None,
            next_status_refresh: Instant::now() + STATUS_REFRESH_INTERVAL,
            composed: None,
            rect: Rect::ZERO,
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

    fn refresh_runtime_status(&mut self) {
        let status = RelayRuntimeStatus::read(&self.config, SystemTime::now());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.structure_dirty = true;
        }
        self.next_status_refresh = Instant::now() + STATUS_REFRESH_INTERVAL;
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let role = Dropdown::new(
            ROLE_OPTIONS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
            ROLE_KEY,
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
                REPLICATION_KEY,
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
            availability_rows.push(value_row("Home peer", self.runtime_status.label()));
        }
        availability_rows.push(value_row(
            "Workspaces",
            &self.config.workspaces.len().to_string(),
        ));
        availability_rows.push(caption(match self.config.device_role {
            DeviceRole::Interactive => {
                "Choose Keep everything available to make this device your always-on sync home."
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
                            Box::new(Toggle::new(&key, &key).with_id(&key)),
                        )
                    })
                    .collect(),
            )
        };
        let pairing = self.pairing_child.is_some();
        let approve = Box::new(
            Button::new(
                "device_mesh.approve_enrollment",
                if pairing {
                    "Pairing…"
                } else {
                    "Pair home server"
                },
                Some(APPROVE_ENROLLMENT_KEY),
            )
            .with_enabled(!pairing),
        );
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
            card("Keep everything available", rows(availability_rows)),
            card("Synced workspaces", workspace_rows),
            card_with_action("Home server or NAS", approve, rows(home_server_rows)),
        ];
        if shows_advanced_services(self.config.device_role) {
            sections.push(card(
                "Advanced",
                rows(vec![
                    setting_row_described(
                        "Collaboration and chat relay",
                        "Keep workspace updates and messages moving between devices",
                        Box::new(
                            Toggle::new("settings.device_mesh.inbound_relay", INBOUND_RELAY_KEY)
                                .with_id("device_mesh.inbound_relay"),
                        ),
                    ),
                    setting_row_described(
                        "Remote-control relay",
                        "Connect remote-control sessions through this device when needed",
                        Box::new(
                            Toggle::new("settings.device_mesh.remote_relay", REMOTE_RELAY_KEY)
                                .with_id("device_mesh.remote_relay"),
                        ),
                    ),
                    setting_row_described(
                        "Serve app backends",
                        "Run compatible app services from this device",
                        Box::new(
                            Toggle::new("settings.device_mesh.app_serving", APP_SERVING_KEY)
                                .with_id("device_mesh.app_serving"),
                        ),
                    ),
                    setting_row_described(
                        "Accept distributed jobs",
                        "Run compatible background jobs from this device",
                        Box::new(
                            Toggle::new("settings.device_mesh.compute", COMPUTE_KEY)
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
        if self.config_path == PathBuf::from(DEFAULT_CONFIG) {
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

    fn approve_enrollment(&mut self) {
        if self.pairing_child.is_some() {
            return;
        }
        let mut command = Command::new("pkexec");
        command
            .args([CONTROL_HELPER, "approve-enrollment", "--identity-root"])
            .arg(identity_root());
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
        for id in ids {
            command.args(["--workspace-id", &id.to_string()]);
        }
        self.load_status = match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                self.pairing_child = Some(child);
                "Home server pairing in progress…".into()
            }
            Err(error) => format!("Could not start pairing: {error}"),
        };
        self.structure_dirty = true;
    }

    fn poll_background_tasks(&mut self) {
        let apply_result = self.apply_child.as_mut().map(poll_child);
        match apply_result {
            Some(Ok(Some(status))) => {
                let mut child = self.apply_child.take().expect("apply child exists");
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
                let mut child = self.pairing_child.take().expect("pairing child exists");
                if status.success() {
                    self.registered_home_peers = registered_home_peer_count();
                    self.refresh_runtime_status();
                    self.load_status = "Home server paired".into();
                } else {
                    self.load_status = format!(
                        "Home server was not paired: {}. Choose Pair home server to retry.",
                        child_error(&mut child, "the pairing window closed")
                    );
                }
                self.structure_dirty = true;
            }
            Some(Err(error)) => {
                self.pairing_child = None;
                self.load_status = format!(
                    "Home server pairing stopped: {error}. Choose Pair home server to retry."
                );
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.poll_background_tasks();
        if self.needs_seed {
            let _ = state.put(root, ROLE_KEY, role_label(self.config.device_role));
            let _ = state.put(
                root,
                REPLICATION_KEY,
                replication_label(&self.common_replication()),
            );
            for (key, value) in [
                (INBOUND_RELAY_KEY, self.config.services.inbound_relay),
                (REMOTE_RELAY_KEY, self.config.services.remote_control_relay),
                (APP_SERVING_KEY, self.config.services.app_serving),
                (COMPUTE_KEY, self.config.services.compute),
            ] {
                let _ = state.put(root, key, value);
            }
            for workspace in &self.discovered_workspaces {
                let selected = self
                    .config
                    .workspaces
                    .iter()
                    .any(|binding| binding.id == workspace.id);
                let _ = state.put(root, workspace_key(workspace.id), selected);
            }
            let _ = state.put(root, APPROVE_ENROLLMENT_KEY, false);
            self.needs_seed = false;
        }

        let mut changed = false;
        if let Some(role) = Self::read_str(state, root, ROLE_KEY)
            .as_deref()
            .and_then(role_from_label)
            && role != self.config.device_role
        {
            if shows_advanced_services(role) != shows_advanced_services(self.config.device_role) {
                self.structure_dirty = true;
            }
            self.config.device_role = role;
            if role.can_serve() {
                self.config.services.workspace_replication = true;
                self.config.services.inbound_relay = true;
                let _ = state.put(root, INBOUND_RELAY_KEY, true);
                for workspace in &mut self.config.workspaces {
                    workspace.pin = true;
                }
            }
            if role == DeviceRole::AlwaysOnServer {
                self.config.services.full_content_replication = true;
                for workspace in &mut self.config.workspaces {
                    workspace.replication = ReplicationIntent::FullWorkspace;
                }
                let _ = state.put(
                    root,
                    REPLICATION_KEY,
                    replication_label(&ReplicationIntent::FullWorkspace),
                );
            }
            self.next_status_refresh = Instant::now();
            changed = true;
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
                self.approve_enrollment();
            }
        }
        if changed {
            self.persist();
            self.structure_dirty = true;
        }
        if Instant::now() >= self.next_status_refresh {
            self.refresh_runtime_status();
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

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own || self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.apply_child.is_some() || self.pairing_child.is_some() {
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
        doc.put(&automerge::ROOT, ROLE_KEY, "Sync while awake")
            .unwrap();
        doc.put(&automerge::ROOT, REPLICATION_KEY, "Keep all files synced")
            .unwrap();
        doc.put(&automerge::ROOT, REMOTE_RELAY_KEY, false).unwrap();
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
        assert_eq!(pane.config.services, Default::default());
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(!path.exists());
        doc.put(&automerge::ROOT, ROLE_KEY, "Sync while awake")
            .unwrap();
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(path.exists());
    }

    #[test]
    fn file_policy_labels_map_to_the_existing_replication_intents() {
        assert_eq!(
            replication_from_label("Continue without files"),
            Some(ReplicationIntent::WorkspaceOnly)
        );
        assert_eq!(
            replication_from_label("Download files when opened"),
            Some(ReplicationIntent::OnDemand)
        );
        assert_eq!(
            replication_from_label("Keep all files synced"),
            Some(ReplicationIntent::FullWorkspace)
        );
        assert_eq!(
            replication_label(&ReplicationIntent::OnDemand),
            "Download files when opened"
        );
        assert_eq!(
            replication_label(&ReplicationIntent::FullWorkspace),
            "Keep all files synced"
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

        doc.put(&automerge::ROOT, ROLE_KEY, "Sync while awake")
            .unwrap();
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
            services: Default::default(),
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
        doc.put(&automerge::ROOT, ROLE_KEY, "Keep everything available")
            .unwrap();
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
        pane.approve_enrollment();
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
