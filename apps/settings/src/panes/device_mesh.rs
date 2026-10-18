//! Devices & Sync — configure seamless continuation and always-on sync.
//!
//! The pane reads and writes the same `RelayConfig` consumed by `tilo-relay`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::time::{Duration, Instant, SystemTime};

use tilo_relay_protocol::config::CheckpointPrefixRecovery;
use tilo_relay_protocol::{DeviceRole, RelayConfig, ReplicationIntent};
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::tilo_actions::remote::community::CommunityStatus;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Dropdown, Text, TextInput, Toggle, Widget};

use crate::continuity::{self, Capability, ContinuityPrivacy};
use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    SettingsSection, StatusTone, card, card_with_action, cards, error_caption, note, row, rows,
    semantic_status_pill, setting_row_described, value_row,
};
use settings_store::roaming_provenance::{Origin, RoamingProvenance};

const DEFAULT_CONFIG: &str = "/etc/tilo/relay.toml";
const CONTINUITY_ENABLED_KEY: &str = "device_mesh.continuity.enabled";
const CONTROL_HELPER: &str = "/usr/libexec/tilo-home-peer-control";
const RELAY_ADMIN: &str = "/usr/bin/tilo-relay-admin";

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;
const APPLY_TIMEOUT: Duration = Duration::from_secs(120);
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
    if status == "Home server paired" {
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
    /// The relay's state could not be read from here, which says nothing
    /// about whether the relay is running.
    Unreadable,
}

impl RelayRuntimeStatus {
    fn read(config: &RelayConfig, now: SystemTime) -> Self {
        // The state directory's parent always exists on a device; when it is
        // not visible the relay cannot be observed, not "offline".
        if config
            .state_root
            .parent()
            .is_some_and(|parent| !parent.is_dir())
        {
            return Self::Unreadable;
        }
        let metrics_path = config
            .state_root
            .join(tilo_relay_protocol::metrics::METRICS_FILE);
        let modified =
            match std::fs::metadata(metrics_path).and_then(|metadata| metadata.modified()) {
                Ok(modified) => modified,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Offline,
                Err(_) => return Self::Unreadable,
            };
        if now.duration_since(modified).unwrap_or(Duration::ZERO) > STATUS_FRESHNESS {
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
            Ok(None) => Self::Offline,
            Err(_) => Self::Unreadable,
        }
    }

    fn label(self) -> String {
        t(match self {
            Self::Offline => "mesh-runtime-offline",
            Self::Online => "mesh-runtime-online",
            Self::Syncing => "mesh-runtime-syncing",
            Self::Ready => "mesh-runtime-ready",
            Self::Unreadable => "mesh-runtime-unreadable",
        })
    }
}

/// How often a shown pane asks a home server how it is, once each has
/// been asked on opening. A home server's health is a network probe with
/// no local change to watch.
const HOME_PEER_HEALTH_INTERVAL: Duration = Duration::from_secs(60);

/// The home-server removals the host still has to deliver, in the
/// identity root.
const PENDING_DECOMMISSIONS: &str = "pending-home-peer-decommissions.json";

/// What a shown Device mesh pane follows: the relay's status, read on a
/// thread of its own, and the files the pane shows.
///
/// The relay rewrites its metrics every second as a heartbeat, so a watch
/// that woke the window for each would be a one-second timer again. The
/// thread re-reads the status on each heartbeat and once its freshness
/// would lapse, and wakes the window only when the status changes.
struct MeshFollow {
    status: Arc<Mutex<Option<RelayRuntimeStatus>>>,
    /// The roaming provenance and the pending home-server removals.
    files: crate::watch::Watched,
    stop: Arc<std::sync::atomic::AtomicBool>,
    poke: std::sync::mpsc::Sender<()>,
}

impl MeshFollow {
    /// Follow `config`'s relay and `files`; `None` when no thread can be
    /// started (the pane then reads on its timer). Must be called on the
    /// UI thread, in the window's wake scope.
    fn start(config: &RelayConfig, files: &[PathBuf]) -> Option<Self> {
        let status = Arc::new(Mutex::new(None));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (poke, events) = std::sync::mpsc::channel();
        let follow = {
            let (config, latest, stop, poke) = (
                config.clone(),
                Arc::clone(&status),
                Arc::clone(&stop),
                poke.clone(),
            );
            let wake = ui::WakeHandle::new();
            Box::new(move || follow_relay(&config, &latest, &stop, poke, &events, &wake))
        };
        crate::device_read::thread("settings-relay-status", follow).ok()?;
        Some(Self {
            status,
            files: crate::watch::Watched::files(files),
            stop,
            poke,
        })
    }

    /// Driven by hand: no thread, files flagged by [`Self::files`]'s touch.
    #[cfg(test)]
    fn by_hand() -> Self {
        Self {
            status: Arc::new(Mutex::new(None)),
            files: crate::watch::Watched::by_hand(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            poke: std::sync::mpsc::channel().0,
        }
    }

    /// The relay's status as the thread last read it.
    fn status(&self) -> Option<RelayRuntimeStatus> {
        self.status.lock().ok().and_then(|status| *status)
    }
}

impl Drop for MeshFollow {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = self.poke.send(());
    }
}

/// The relay-status thread: read the status, publish it to `latest` and
/// wake the window when it changed, then wait for the relay's next write
/// (or for its freshness to lapse) until `stop`.
fn follow_relay(
    config: &RelayConfig,
    latest: &Mutex<Option<RelayRuntimeStatus>>,
    stop: &std::sync::atomic::AtomicBool,
    poke: std::sync::mpsc::Sender<()>,
    events: &std::sync::mpsc::Receiver<()>,
    wake: &ui::WakeHandle,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let heartbeat = [
        config
            .state_root
            .join(tilo_relay_protocol::metrics::METRICS_FILE),
        config.full_replica_status.clone(),
    ];
    let on_change: crate::watch::OnChange = Arc::new(move || {
        let _ = poke.send(());
    });
    let mut watch = crate::watch::Watched::calling(&heartbeat, &on_change);
    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        let status = RelayRuntimeStatus::read(config, ui::time::wall());
        if latest
            .lock()
            .is_ok_and(|mut latest| latest.replace(status) != Some(status))
        {
            wake.wake();
        }
        let wait = if !watch.is_complete() {
            // Not there to watch yet (no relay ran here): look again.
            Some(STATUS_REFRESH_INTERVAL)
        } else if matches!(
            status,
            RelayRuntimeStatus::Offline | RelayRuntimeStatus::Unreadable
        ) {
            None
        } else {
            // Without another write the status goes stale after this.
            Some(STATUS_FRESHNESS + Duration::from_secs(1))
        };
        let event = match wait {
            Some(wait) => events.recv_timeout(wait),
            None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match event {
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if !watch.is_complete() => {
                watch = crate::watch::Watched::calling(&heartbeat, &on_change);
            }
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn runtime_status_tone(status: RelayRuntimeStatus) -> StatusTone {
    match status {
        RelayRuntimeStatus::Ready | RelayRuntimeStatus::Online => StatusTone::Success,
        RelayRuntimeStatus::Syncing | RelayRuntimeStatus::Unreadable => StatusTone::Warning,
        RelayRuntimeStatus::Offline => StatusTone::Neutral,
    }
}

/// This device's place among the person's devices: what it keeps, serves
/// and continues, and the devices and home servers it trusts.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DeviceMeshEffect {
    /// How available this device is to the person's other devices. A
    /// serving device takes on system services, so this asks first.
    #[effect(destructive, idempotent)]
    SetDeviceRole {
        #[param("Personal device, Available while active, or Always-on server", choices = DEVICE_ROLES)]
        role: String,
    },
    /// How much of each synced workspace this device keeps.
    #[effect(idempotent)]
    SetReplication {
        #[param("On demand, Workspace state, or Full workspace copy", choices = REPLICATIONS)]
        replication: String,
    },
    /// Offer or stop one service of a serving device.
    #[effect(idempotent)]
    SetDeviceService {
        #[param("inbound_relay, remote_control_relay, app_serving or compute", choices = SERVICES)]
        service: String,
        on: bool,
    },
    /// Keep a discovered workspace on this device, or stop keeping it.
    #[effect(row, idempotent)]
    SetSyncedWorkspace {
        #[param("The workspace's id")]
        workspace: u64,
        on: bool,
    },
    /// Look for a new home server nearby, then approve the one found.
    #[effect(label = "Pair home server")]
    PairHomeServer,
    /// Look for a new personal device nearby, then approve the one found.
    #[effect(label = "Add personal device")]
    AddPersonalDevice,
    /// End a personal device's access to the person's workspaces.
    #[effect(job, row, destructive, label = "Revoke access")]
    RevokeDevice {
        #[param("The device's key, in hex")]
        device: String,
        #[param("Retired, Lost, Stolen or Compromised", choices = REVOKE_REASONS.iter().copied())]
        reason: String,
    },
    /// Rename a home server.
    #[effect(row)]
    RenameHomeServer {
        #[param("The home server's node id")]
        device: String,
        #[param("Its new name")]
        name: String,
    },
    /// Remove a home server; it stops serving the person's workspaces.
    #[effect(row, destructive, label = "Remove home server")]
    RemoveHomeServer {
        #[param("The home server's node id")]
        device: String,
    },
    /// Join the site's community workspace again after leaving it.
    #[effect(job, label = "Join community workspace")]
    JoinCommunity,
    /// Leave the site's community workspace; it is not joined again unasked.
    #[effect(job, destructive, label = "Leave community workspace")]
    LeaveCommunity,
}

const DEVICE_ROLES: [&str; 3] = [ROLE_PERSONAL, ROLE_ACTIVE, ROLE_SERVER];
const REPLICATIONS: [&str; 3] = [
    REPLICATION_ON_DEMAND,
    REPLICATION_WORKSPACE,
    REPLICATION_FULL,
];
const SERVICES: [&str; 4] = [
    "inbound_relay",
    "remote_control_relay",
    "app_serving",
    "compute",
];

/// Answer `effect` on the model: configuration is written at once,
/// pairing and server management run as bounded background commands.
#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DeviceMeshEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.device_mesh().model();
    let mut model = model.borrow_mut();
    // A revocation is signed by the host; its caller hears how it ended.
    if let DeviceMeshEffect::RevokeDevice { device, reason } = &effect {
        let device = model.revocable(device)?;
        model.start_revoke(&device, reason, Some(cx.reply()));
        return Ok(Effected::Pending);
    }
    // Joining and leaving run on the host; joining can take seconds.
    if matches!(
        effect,
        DeviceMeshEffect::JoinCommunity | DeviceMeshEffect::LeaveCommunity
    ) {
        let join = effect == DeviceMeshEffect::JoinCommunity;
        model.start_community_change(join, Some(cx.reply()))?;
        return Ok(Effected::Pending);
    }
    model.apply(&effect).map(|()| Effected::done())
}

/// What the Devices & Sync area keeps for the window: the model, read when
/// first needed.
pub struct DeviceMeshArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<DeviceMeshModel>>,
    /// The relay's configuration and state, read off the UI thread after
    /// the window's first frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<MeshRead>,
    /// The area's client for the action door, shared with its model: the
    /// window carries its requests and answers on the app's connection.
    remote: ui::RemoteActions,
}

impl DeviceMeshArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
            remote: ui::RemoteActions::current(),
        }
    }

    /// The client the window's connection carries pairing requests on.
    pub(crate) fn remote_mut(&mut self) -> &mut ui::RemoteActions {
        &mut self.remote
    }

    /// The same client, for another area that acts through the host.
    pub(crate) fn remote(&self) -> ui::RemoteActions {
        self.remote.clone()
    }

    /// Where the relay configuration is read from, and whether what is
    /// paired or discovered is: this device's, or, for a screenshot, an
    /// unconfigured one with nothing paired.
    fn source(&self) -> (PathBuf, bool) {
        if self.cx.reaches_devices() {
            (PathBuf::from(DEFAULT_CONFIG), true)
        } else {
            (PathBuf::from("/nonexistent/tilo/relay.toml"), false)
        }
    }

    /// The model, over this device's relay configuration, or, for a
    /// screenshot, over an unconfigured one with nothing paired: from the
    /// read [`Self::start_read`] started, waiting for it if it still runs,
    /// else read here.
    pub fn model(&self) -> &Shared<DeviceMeshModel> {
        self.model.get_or_init(|| {
            let read = self.read.take().unwrap_or_else(|| {
                let (path, devices) = self.source();
                MeshRead::of(path, devices, ui::time::wall())
            });
            let mut model = DeviceMeshModel::from_read(self.cx.clone(), read);
            model.remote = self.remote.clone();
            // The fleet and the community workspace are the host's: it
            // answers both; a capture shows its stand-ins.
            if self.cx.reaches_devices() {
                model.refresh_personal_devices();
                model.refresh_community();
            } else if let Some(scene) = self.cx.scene() {
                model.community.clone_from(&scene.community);
                match crate::device_fleet::views(scene.fleet_devices.clone()) {
                    Ok(devices) => model.personal_devices = devices,
                    Err(error) => model.personal_device_error = Some(error),
                }
            }
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Read the relay off the UI thread; `false` when
    /// they have been read or are being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let (path, devices) = self.source();
        let now = ui::time::wall();
        self.read.start(move || MeshRead::of(path, devices, now))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Take in finished background commands, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_background_tasks();
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per discovered workspace, personal device and home server.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let offer = |effect: DeviceMeshEffect| SettingsEffect::from(effect).bound();
        let workspaces = model.discovered_workspaces.iter().map(|workspace| {
            let kept = model
                .config
                .workspaces
                .iter()
                .any(|binding| binding.id == workspace.id);
            LiveRow::new(
                format!("settings.device-mesh-workspace:{}", workspace.id),
                "settings.device-mesh-workspace",
                workspace.name.clone(),
            )
            .operation(offer(DeviceMeshEffect::SetSyncedWorkspace {
                workspace: workspace.id,
                on: !kept,
            }))
        });
        let devices = model
            .personal_devices
            .iter()
            .filter(|device| !device.current && !device.revoked)
            .map(|device| {
                LiveRow::new(
                    format!("settings.personal-device:{}", hex::encode(device.key)),
                    "settings.personal-device",
                    device.label.clone(),
                )
                .operation(offer(DeviceMeshEffect::RevokeDevice {
                    device: hex::encode(device.key),
                    reason: "Retired".into(),
                }))
            });
        let servers = model.home_peers.iter().map(|peer| {
            LiveRow::new(
                format!("settings.home-server:{}", peer.device_node_id),
                "settings.home-server",
                peer.label.clone(),
            )
            .operation(offer(DeviceMeshEffect::RemoveHomeServer {
                device: peer.device_node_id.clone(),
            }))
        });
        workspaces.chain(devices).chain(servers).collect()
    }
}

/// Editor for the daemon's real device-mesh configuration.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: what is being saved, paired and delivered, and whether it changed"
)]
pub struct DeviceMeshModel {
    config_path: PathBuf,
    config: RelayConfig,
    load_status: String,
    cx: PaneCx,
    changed: bool,
    runtime_status: RelayRuntimeStatus,
    discovered_workspaces: Vec<DiscoveredWorkspace>,
    registered_home_peers: usize,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
    home_peer_health: HashMap<String, String>,
    home_peer_last_seen: HashMap<String, u64>,
    personal_devices: Vec<crate::device_fleet::DeviceView>,
    personal_device_error: Option<String>,
    /// The continuity choices as last read, and the person's settings
    /// holding them.
    continuity: ContinuityPrivacy,
    continuity_settings: Followed<ContinuityPrivacy>,
    /// The name typed for each home server, by node id.
    peer_names: HashMap<String, Arc<Mutex<String>>>,
    roaming_provenance_path: Option<PathBuf>,
    roaming_provenance_stamp: Option<SystemTime>,
    roaming_provenance: Option<RoamingProvenance>,
    roaming_provenance_status: Option<String>,
    management_child: Option<bounded_command::AsyncCommand>,
    management_device: Option<String>,
    /// A home-server removal the host runs for this pane, and the server:
    /// it signs with the root key, so it never runs here.
    management_call: Option<(ui::LocalState<ui::InvocationCall>, String)>,
    /// The host's answer to the device list, while it is asked.
    devices_call: Option<ui::LocalState<ui::InvocationCall>>,
    /// The revocation the host signs: its invocation, the device's name and
    /// the caller waiting for it.
    revoke_call: Option<RevokeCall>,
    /// Where the person stands with the site's community workspace, as the
    /// host last answered; `None` until it did.
    community: Option<CommunityStatus>,
    /// Why the community workspace could not be read, joined or left: the
    /// message's key and the host's reason.
    community_error: Option<(&'static str, String)>,
    /// The host's answer to the community status, while it is asked.
    community_call: Option<ui::LocalState<ui::InvocationCall>>,
    /// The join or leave the host runs: its invocation, which it is and the
    /// caller waiting for it.
    community_change: Option<CommunityChange>,
    next_peer_health: usize,
    apply_child: Option<bounded_command::AsyncCommand>,
    apply_queued: bool,
    /// The pairing step the host runs for this pane: its invocation, and
    /// whether it is the discovery (else the approval).
    pairing_call: Option<ui::LocalState<ui::InvocationCall>>,
    pairing_discovery: bool,
    pairing_candidate: Option<PairCandidate>,
    pairing_target: PairingTarget,
    /// The pane's client for the action door: pairing runs on the host,
    /// behind its actions, never in this process.
    remote: ui::RemoteActions,
    /// When the status is read again while no pane follows it; while one
    /// does, when the next home server is asked how it is.
    next_status_refresh: Instant,
    /// While a pane shows the area: the relay's status and the files the
    /// pane shows, followed for it.
    following: std::rc::Weak<MeshFollow>,
    /// Home servers still to ask on opening, one after another.
    health_round: usize,
    /// A home server's removal still waits to be delivered; it is retried
    /// with the health checks.
    removals_pending: bool,
}

/// A device revocation the host is signing.
struct RevokeCall {
    call: ui::LocalState<ui::InvocationCall>,
    label: String,
    reply: Option<EffectReply>,
}

/// Joining or leaving the community workspace, while the host runs it.
struct CommunityChange {
    call: ui::LocalState<ui::InvocationCall>,
    join: bool,
    reply: Option<EffectReply>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredWorkspace {
    id: u64,
    name: String,
}

/// The device the host's discovery found, as its action answers it.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
struct PairCandidate {
    device: String,
    service: String,
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

fn registered_home_peer_count() -> usize {
    registered_home_peers().len()
}

fn registered_home_peers() -> Vec<tilo_relay_protocol::HomePeerRegistration> {
    std::fs::read(tilo_relay_protocol::home_peer_registry_path(
        &identity_root(),
    ))
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
    format!("{} at revision {}", origin.device_label, origin.counter)
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

/// The roaming provenance at `path` as read: the projection or why it
/// could not be read, and when the file was last written.
type ProvenanceRead = (
    Option<RoamingProvenance>,
    Option<String>,
    Option<SystemTime>,
);

/// Read the roaming provenance at `path`; an error is left untranslated
/// ([`shown_roaming_provenance`]), so this may run off the UI thread.
fn read_roaming_provenance(path: Option<&std::path::Path>) -> ProvenanceRead {
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
        Err(error) => (None, Some(error.to_string()), stamp),
    }
}

/// `read` as the pane shows it: the projection, the status saying why it
/// could not be read, and its stamp.
fn shown_roaming_provenance(read: ProvenanceRead) -> ProvenanceRead {
    let (provenance, error, stamp) = read;
    let status = error.map(|error| t_args("mesh-provenance-err", &[("error", error.into())]));
    (provenance, status, stamp)
}

fn load_roaming_provenance(path: Option<&std::path::Path>) -> ProvenanceRead {
    shown_roaming_provenance(read_roaming_provenance(path))
}

/// The reason the host's revocation takes for the label `label` shows.
fn revocation_reason(label: &str) -> &'static str {
    if label == "Lost" || label == t("mesh-revoke-lost") {
        "lost"
    } else if label == "Stolen" || label == t("mesh-revoke-stolen") {
        "stolen"
    } else if label == "Compromised" || label == t("mesh-revoke-compromised") {
        "compromised"
    } else {
        "retired"
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
    let now_ms = ui::time::unix_now_ms();
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

/// What the Devices & Sync area reads of the device: the relay's
/// configuration and state, and the workspaces and home servers it knows.
/// Read off the UI thread after the window's first frame; the model is
/// built from it on the UI thread ([`DeviceMeshModel::from_read`]), which
/// asks the host for the device fleet.
pub(crate) struct MeshRead {
    config_path: PathBuf,
    config: RelayConfig,
    load_status: String,
    runtime_status: RelayRuntimeStatus,
    discovered_workspaces: Vec<DiscoveredWorkspace>,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
    home_peer_last_seen: HashMap<String, u64>,
    roaming_provenance_path: Option<PathBuf>,
    roaming_provenance: ProvenanceRead,
}

impl MeshRead {
    /// Load the relay configuration at `config_path`, the relay's state as
    /// of `now`, and, with `devices`, what is paired or discovered. Missing
    /// configuration is a normal first-run state and starts from the
    /// daemon's defaults.
    fn of(config_path: PathBuf, devices: bool, now: SystemTime) -> Self {
        let (config, load_status) = match RelayConfig::load(&config_path) {
            Ok(config) => (config, "Configuration loaded".to_string()),
            Err(tilo_relay_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && config_path.parent().is_some_and(|dir| !dir.is_dir()) =>
            {
                // The configuration directory ships with every device; not
                // seeing it means it cannot be read from here, not that the
                // device is unconfigured.
                let config = RelayConfig {
                    device_role: DeviceRole::Interactive,
                    services: tilo_relay_protocol::ServiceCapabilities::default(),
                    ..RelayConfig::default()
                };
                let dir = config_path
                    .parent()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default();
                (
                    config,
                    format!("Configuration could not be read: {dir} is not visible to Settings"),
                )
            }
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
        let runtime_status = RelayRuntimeStatus::read(&config, now);
        let discovered_workspaces = if devices {
            discover_workspaces(&config)
        } else {
            Vec::new()
        };
        let home_peers = if devices {
            registered_home_peers()
        } else {
            Vec::new()
        };
        let roaming_provenance_path = devices
            .then(settings_store::roaming_provenance::path)
            .flatten();
        let roaming_provenance = read_roaming_provenance(roaming_provenance_path.as_deref());
        Self {
            config_path,
            config,
            load_status,
            runtime_status,
            discovered_workspaces,
            home_peers,
            home_peer_last_seen: if devices {
                load_home_peer_last_seen()
            } else {
                HashMap::new()
            },
            roaming_provenance_path,
            roaming_provenance,
        }
    }
}

impl DeviceMeshModel {
    /// Load the relay configuration at `config_path` here
    /// ([`MeshRead::of`]). Without `devices`, nothing paired or discovered
    /// is read.
    #[cfg(test)]
    fn from_path(cx: PaneCx, config_path: PathBuf, devices: bool) -> Self {
        Self::from_read(cx, MeshRead::of(config_path, devices, ui::time::wall()))
    }

    /// The model over what `read` read of the device.
    fn from_read(cx: PaneCx, read: MeshRead) -> Self {
        let continuity_settings = cx.follow::<ContinuityPrivacy>();
        let continuity = continuity_settings.get().clone();
        let (roaming_provenance, roaming_provenance_status, roaming_provenance_stamp) =
            shown_roaming_provenance(read.roaming_provenance);
        Self {
            config_path: read.config_path,
            config: read.config,
            load_status: read.load_status,
            cx,
            changed: false,
            runtime_status: read.runtime_status,
            discovered_workspaces: read.discovered_workspaces,
            registered_home_peers: read.home_peers.len(),
            home_peers: read.home_peers,
            home_peer_health: HashMap::new(),
            home_peer_last_seen: read.home_peer_last_seen,
            personal_devices: Vec::new(),
            personal_device_error: None,
            continuity,
            continuity_settings,
            peer_names: HashMap::new(),
            roaming_provenance_path: read.roaming_provenance_path,
            roaming_provenance_stamp,
            roaming_provenance,
            roaming_provenance_status,
            management_child: None,
            management_device: None,
            management_call: None,
            devices_call: None,
            revoke_call: None,
            community: None,
            community_error: None,
            community_call: None,
            community_change: None,
            next_peer_health: 0,
            apply_child: None,
            apply_queued: false,
            pairing_call: None,
            pairing_discovery: false,
            pairing_candidate: None,
            pairing_target: PairingTarget::HomePeer,
            remote: ui::RemoteActions::current(),
            next_status_refresh: ui::time::now() + STATUS_REFRESH_INTERVAL,
            following: std::rc::Weak::new(),
            health_round: 0,
            removals_pending: false,
        }
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        let cx = PaneCx::in_file(path.with_file_name("settings.json"));
        let mut pane = Self::from_path(cx, path.to_path_buf(), false);
        pane.roaming_provenance_path =
            Some(path.with_file_name(settings_store::roaming_provenance::FILE_NAME));
        pane.refresh_roaming_provenance();
        // Swapping the per-user provenance file read above for the fixture's
        // is construction, not a change the host must rebuild for.
        pane.changed = false;
        // The periodic runtime-status poll is wall-clock driven; on a loaded
        // machine its interval can elapse between construction and the first
        // sync and flag a rebuild no test action caused.
        pane.next_status_refresh = ui::time::now() + Duration::from_secs(24 * 60 * 60);
        pane
    }

    fn common_replication(&self) -> ReplicationIntent {
        self.config
            .workspaces
            .first()
            .map_or(ReplicationIntent::OnDemand, |workspace| {
                workspace.replication.clone()
            })
    }

    /// Carry out `effect`.
    fn apply(&mut self, effect: &DeviceMeshEffect) -> Result<(), String> {
        let configured = match effect {
            DeviceMeshEffect::SetDeviceRole { role } => {
                let role =
                    role_from_label(role).ok_or_else(|| "unsupported device role".to_string())?;
                self.apply_role_change(role);
                true
            }
            DeviceMeshEffect::SetReplication { replication } => {
                let intent = replication_from_label(replication)
                    .ok_or_else(|| "unsupported replication policy".to_string())?;
                for workspace in &mut self.config.workspaces {
                    workspace.replication = intent.clone();
                }
                self.config.services.full_content_replication =
                    intent == ReplicationIntent::FullWorkspace;
                true
            }
            DeviceMeshEffect::SetDeviceService { service, on } => {
                match service.as_str() {
                    "inbound_relay" => self.config.services.inbound_relay = *on,
                    "remote_control_relay" => self.config.services.remote_control_relay = *on,
                    "app_serving" => self.config.services.app_serving = *on,
                    "compute" => self.config.services.compute = *on,
                    _ => return Err("unsupported device service".into()),
                }
                true
            }
            DeviceMeshEffect::SetSyncedWorkspace { workspace, on } => {
                let id = *workspace;
                if !self
                    .discovered_workspaces
                    .iter()
                    .any(|workspace| workspace.id == id)
                {
                    return Err("workspace is no longer available on this device".into());
                }
                if *on
                    && !self
                        .config
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == id)
                {
                    self.config
                        .workspaces
                        .push(tilo_relay_protocol::WorkspaceBinding {
                            id,
                            pin: self.config.device_role.can_serve(),
                            replication: ReplicationIntent::OnDemand,
                            admitted_users: Vec::new(),
                            // Workspace discovery proves only that the room exists. It
                            // cannot confer the native shell governor's retained authority.
                            shell_authority: None,
                            checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
                        });
                } else if !*on {
                    self.config.remove_workspace(id);
                }
                true
            }
            DeviceMeshEffect::PairHomeServer => {
                self.approve_enrollment(PairingTarget::HomePeer);
                false
            }
            DeviceMeshEffect::AddPersonalDevice => {
                self.approve_enrollment(PairingTarget::PersonalDevice);
                false
            }
            DeviceMeshEffect::RevokeDevice { device, reason } => {
                let device = self.revocable(device)?;
                self.start_revoke(&device, reason, None);
                false
            }
            DeviceMeshEffect::RenameHomeServer { device, name } => {
                let peer = self.home_peer(device)?;
                if !tilo_relay_protocol::valid_home_peer_label(name) {
                    return Err("that name cannot be used for a home server".into());
                }
                self.start_home_peer_command(&peer, "home-peer-rename", Some(name.trim()));
                false
            }
            DeviceMeshEffect::RemoveHomeServer { device } => {
                let peer = self.home_peer(device)?;
                self.start_home_peer_removal(&peer);
                false
            }
            DeviceMeshEffect::JoinCommunity => {
                self.start_community_change(true, None)?;
                false
            }
            DeviceMeshEffect::LeaveCommunity => {
                self.start_community_change(false, None)?;
                false
            }
        };
        if configured {
            self.persist();
        }
        self.changed = true;
        Ok(())
    }

    /// The trusted device `device` names, other than this one.
    fn revocable(&self, device: &str) -> Result<crate::device_fleet::DeviceView, String> {
        self.personal_devices
            .iter()
            .find(|known| hex::encode(known.key) == device && !known.current && !known.revoked)
            .cloned()
            .ok_or_else(|| "that device is no longer trusted here".to_string())
    }

    /// Ask the host to revoke `device` for the reason `reason` shows; the
    /// answer, and `reply`'s, arrive in [`Self::poll_host_calls`].
    fn start_revoke(
        &mut self,
        device: &crate::device_fleet::DeviceView,
        reason: &str,
        reply: Option<EffectReply>,
    ) {
        if let Some(previous) = self.revoke_call.take()
            && let Some(reply) = previous.reply
        {
            reply.finish(Err("another revocation replaced it".into()));
        }
        self.revoke_call = Some(RevokeCall {
            call: crate::device_fleet::revoke(&self.remote, &device.key, revocation_reason(reason)),
            label: device.label.clone(),
            reply,
        });
        self.load_status = format!("Revoking {}…", device.label);
        self.changed = true;
    }

    /// Ask the host again which devices are trusted.
    fn refresh_personal_devices(&mut self) {
        if self.devices_call.is_none() {
            self.devices_call = Some(crate::device_fleet::list(&self.remote));
        }
    }

    /// Ask the host again where the person stands with the community
    /// workspace.
    fn refresh_community(&mut self) {
        if self.community_call.is_none() {
            self.community_call = Some(crate::community::status(&self.remote));
        }
    }

    /// Ask the host to join the community workspace again (`join`) or to
    /// leave it; the answer, and `reply`'s, arrive in
    /// [`Self::poll_host_calls`].
    fn start_community_change(
        &mut self,
        join: bool,
        reply: Option<EffectReply>,
    ) -> Result<(), String> {
        let joined = match &self.community {
            Some(CommunityStatus {
                site: Some(_),
                joined,
                ..
            }) => *joined,
            _ => return Err("this computer has no community workspace".into()),
        };
        if join == joined {
            return Err(if join {
                "this computer is already in the community workspace".into()
            } else {
                "this computer is not in the community workspace".into()
            });
        }
        if let Some(previous) = self.community_change.take()
            && let Some(reply) = previous.reply
        {
            reply.finish(Err("another community change replaced it".into()));
        }
        self.community_change = Some(CommunityChange {
            call: if join {
                crate::community::join(&self.remote)
            } else {
                crate::community::leave(&self.remote)
            },
            join,
            reply,
        });
        self.community_error = None;
        self.changed = true;
        Ok(())
    }

    /// Take in what the host answered to the device list, a revocation, a
    /// home-server removal and the community status, join or leave, once
    /// each settled.
    fn poll_host_calls(&mut self) {
        if self.devices_call.is_some()
            || self.revoke_call.is_some()
            || self.management_call.is_some()
            || self.community_call.is_some()
            || self.community_change.is_some()
        {
            self.remote.poll();
        }
        if let Some(answer) = self
            .devices_call
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.devices_call = None;
            match answer.and_then(|output| crate::device_fleet::devices(output.value)) {
                Ok(devices) => {
                    self.personal_devices = devices;
                    self.personal_device_error = None;
                }
                Err(error) => self.personal_device_error = Some(error),
            }
            self.changed = true;
        }
        if let Some(answer) = self
            .revoke_call
            .as_ref()
            .and_then(|revoke| crate::device_fleet::settled(&revoke.call))
            && let Some(RevokeCall { label, reply, .. }) = self.revoke_call.take()
        {
            self.load_status = match &answer {
                Ok(_) => format!("{label} can no longer access your workspaces"),
                Err(error) => format!("{label} was not revoked: {error}"),
            };
            if let Some(reply) = reply {
                reply.finish(answer);
            }
            self.refresh_personal_devices();
            self.changed = true;
        }
        if let Some(answer) = self
            .management_call
            .as_ref()
            .and_then(|(call, _)| crate::device_fleet::settled(call))
            && let Some((_, device)) = self.management_call.take()
        {
            self.take_management_answer(device, answer.map(|output| output.value));
        }
        if let Some(answer) = self
            .community_call
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.community_call = None;
            match answer.and_then(|output| crate::community::parse(output.value)) {
                Ok(status) => {
                    self.community = Some(status);
                    self.community_error = None;
                }
                Err(error) => self.community_error = Some(("mesh-community-err", error)),
            }
            self.changed = true;
        }
        if let Some(answer) = self
            .community_change
            .as_ref()
            .and_then(|change| crate::device_fleet::settled(&change.call))
            && let Some(CommunityChange { join, reply, .. }) = self.community_change.take()
        {
            // Both answer where the person stands now; the status is read
            // again all the same, so a joining that settles later shows.
            match &answer {
                Ok(output) => {
                    if let Ok(status) = crate::community::parse(output.value.clone()) {
                        self.community = Some(status);
                    }
                }
                Err(error) => {
                    let key = if join {
                        "mesh-community-join-err"
                    } else {
                        "mesh-community-leave-err"
                    };
                    self.community_error = Some((key, error.clone()));
                }
            }
            if let Some(reply) = reply {
                reply.finish(answer);
            }
            self.refresh_community();
            self.changed = true;
        }
    }

    /// Ask the host to root-revoke home server `peer` and stop it serving
    /// the person's workspaces. Choosing the destructive row is the
    /// person's confirmation.
    fn start_home_peer_removal(&mut self, peer: &tilo_relay_protocol::HomePeerRegistration) {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        if self.management_child.is_some() || self.management_call.is_some() {
            return;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("device".into(), peer.device_node_id.clone().into());
        arguments.insert("service".into(), peer.relay_service_node_id.clone().into());
        arguments.insert("confirm".into(), true.into());
        let call = self.remote.invoke(
            identity_admin::target(identity_admin::HOME_PEER_REMOVE),
            InvokeArgument::Json(arguments),
            Some(Confirmation::Confirmed),
        );
        self.management_call = Some((call, peer.device_node_id.clone()));
        self.home_peer_health
            .insert(peer.device_node_id.clone(), "Connecting…".into());
        self.changed = true;
    }

    /// Take in how a home-server command for `device` ended: the response
    /// it reported, nothing, or why it failed.
    fn take_management_answer(
        &mut self,
        device: String,
        answer: Result<Option<serde_json::Value>, String>,
    ) {
        match answer {
            Ok(Some(response)) => {
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
                        "Online. Files ready".into()
                    } else {
                        "Online. Syncing files".into()
                    },
                );
                let now_ms = ui::time::unix_now_ms();
                self.home_peer_last_seen.insert(device, now_ms);
                save_home_peer_last_seen(&self.home_peer_last_seen);
                self.home_peers = registered_home_peers();
                self.registered_home_peers = self.home_peers.len();
            }
            Ok(None) => {}
            Err(detail) => {
                let label = self.home_peer_last_seen.get(&device).map_or_else(
                    || format!("Offline: {detail}"),
                    |last_seen| last_seen_label(*last_seen),
                );
                self.home_peer_health.insert(device, label);
            }
        }
        self.changed = true;
    }

    fn home_peer(&self, device: &str) -> Result<tilo_relay_protocol::HomePeerRegistration, String> {
        self.home_peers
            .iter()
            .find(|peer| peer.device_node_id == device)
            .cloned()
            .ok_or_else(|| "that home server is no longer paired".to_string())
    }

    fn peer_name(&self, peer: &tilo_relay_protocol::HomePeerRegistration) -> Arc<Mutex<String>> {
        self.peer_names
            .get(&peer.device_node_id)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(peer.label.clone())))
    }

    fn refresh_runtime_status(&mut self) {
        let status = RelayRuntimeStatus::read(&self.config, ui::time::wall());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.changed = true;
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
        self.changed = true;
    }

    /// The site's community workspace: where the person stands and the
    /// button that joins it again or leaves it; nothing when this computer
    /// has no community workspace.
    fn community_section(&self) -> Option<SettingsSection> {
        let status = self.community.as_ref()?;
        let site = crate::community::site_name(status.site.as_deref()?);
        let changing = self.community_change.as_ref().map(|change| change.join);
        let state = t(match changing {
            Some(false) => "mesh-community-leaving",
            None if status.joined => "mesh-community-in",
            None if status.left => "mesh-community-left",
            // Joining when asked, or joining on its own until it settles.
            Some(true) | None => "mesh-community-joining",
        });
        let detail = t_args(
            "mesh-community-detail",
            &[("site", site.into()), ("state", state.into())],
        );
        let button = if status.joined {
            self.cx.bind_invocation(
                Button::new("device_mesh.community_leave", &t("mesh-community-leave"))
                    .role(ui::widget::ButtonRole::Destructive)
                    .with_enabled(changing.is_none()),
                DeviceMeshEffect::LeaveCommunity,
            )
        } else {
            self.cx.bind_invocation(
                Button::new("device_mesh.community_join", &t("mesh-community-join"))
                    .with_enabled(changing.is_none()),
                DeviceMeshEffect::JoinCommunity,
            )
        };
        let mut community_rows = vec![setting_row_described(
            &t("mesh-community-workspace"),
            &detail,
            Box::new(button),
        )];
        if let Some((key, error)) = &self.community_error {
            community_rows.push(note(&t_args(key, &[("error", error.as_str().into())])));
        }
        Some(card(&t("mesh-community-title"), rows(community_rows)))
    }

    fn apply_role_change(&mut self, role: DeviceRole) -> bool {
        if role == self.config.device_role {
            return false;
        }
        if shows_advanced_services(role) != shows_advanced_services(self.config.device_role) {
            self.changed = true;
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
        let role = self
            .cx
            .bind(role, effect(DeviceMeshEffect::SET_DEVICE_ROLE));

        let activity = t("mesh-activity");
        let sync_auto = t("mesh-sync-auto");
        let files = t("mesh-files");
        let download_opened = t("mesh-download-opened");
        let mut continuation_rows = vec![
            value_row(&activity, &sync_auto),
            note(&t("mesh-activity-caption")),
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
            let replication = self
                .cx
                .bind(replication, effect(DeviceMeshEffect::SET_REPLICATION));
            let repl_desc = replication_description(&self.common_replication());
            continuation_rows.push(setting_row_described(
                &files,
                &repl_desc,
                Box::new(replication),
            ));
        }
        if self.load_status.starts_with("Configuration could not")
            || self.load_status.starts_with("Not saved")
            || self.load_status.starts_with("Could not start pairing")
        {
            continuation_rows.push(row(error_caption(&mesh_status_shown(&self.load_status))));
        } else if self.load_status.starts_with("Waiting") {
            continuation_rows.push(note(&mesh_status_shown(&self.load_status)));
        }

        let this_device = t("mesh-this-device");
        let this_device_desc = t("mesh-this-device-desc");
        let mut availability_rows = vec![setting_row_described(
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
                RelayRuntimeStatus::Unreadable => "mesh-home-unreadable",
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
        availability_rows.push(note(&t(match self.config.device_role {
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
            rows(vec![note(&t("mesh-workspaces-empty"))])
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
                            Box::new({
                                let id = workspace.id;
                                self.cx.bind(
                                    Toggle::from_state(&ui::LocalState::new(selected))
                                        .with_id(&key),
                                    effect(DeviceMeshEffect::SET_SYNCED_WORKSPACE)
                                        .map(move |on: bool| (id, on)),
                                )
                            }),
                        )
                    })
                    .collect(),
            )
        };
        let pairing = self.pairing_call.is_some();
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
        let approve = Box::new(self.cx.bind_invocation(
            Button::new("device_mesh.approve_enrollment", &pair_label).with_enabled(!pairing),
            DeviceMeshEffect::PairHomeServer,
        ));
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
            self.cx.bind_invocation(
                Button::new("device_mesh.approve_personal_device", &personal_label)
                    .with_enabled(!pairing),
                DeviceMeshEffect::AddPersonalDevice,
            ),
        );
        let mut personal_rows = vec![note(&t("mesh-personal-caption"))];
        if self.pairing_target == PairingTarget::PersonalDevice
            && (pairing || self.pairing_candidate.is_some() || !self.load_status.is_empty())
        {
            personal_rows.push(note(&mesh_status_shown(&self.load_status)));
        }
        let mut enrolled_device_rows = Vec::new();
        for device in &self.personal_devices {
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
                if device.current || device.revoked || device.can_host {
                    semantic_status_pill(
                        &status,
                        if device.current {
                            StatusTone::Success
                        } else {
                            StatusTone::Neutral
                        },
                    )
                } else {
                    let key = hex::encode(device.key);
                    Box::new(
                        self.cx.bind(
                            Dropdown::from_state(
                                REVOKE_REASONS.iter().map(|id| revoke_shown(id)).collect(),
                                &ui::LocalState::new(String::new()),
                            )
                            .with_id(&reason_key)
                            .with_accessible_name(t("mesh-why-ending"))
                            .with_placeholder(&t("mesh-decommission")),
                            effect(DeviceMeshEffect::REVOKE_DEVICE)
                                .map(move |reason: String| (key.clone(), reason)),
                        ),
                    )
                },
            ));
        }
        if let Some(error) = &self.personal_device_error {
            enrolled_device_rows.push(note(&t_args(
                "mesh-device-list-err",
                &[("error", error.as_str().into())],
            )));
        } else if enrolled_device_rows.is_empty() {
            enrolled_device_rows.push(note(&t("mesh-device-list-empty")));
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
        let mut privacy_rows = vec![
            self.cx
                .row(t("mesh-allow-continuity"))
                .described(t("mesh-allow-continuity-desc"))
                .id(CONTINUITY_ENABLED_KEY)
                .toggle_verb(
                    &ui::LocalState::new(self.continuity.enabled),
                    ContinuityPrivacy::ALLOW_CONTINUITY,
                ),
        ];
        for capability in Capability::ALL {
            let key = continuity_key(capability);
            let desc = continuity::description(capability);
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
            let facet = capability.key().to_owned();
            privacy_rows.push(
                self.cx
                    .row(continuity::label(capability))
                    .described(status)
                    .id(key)
                    .toggle_verb(
                        &ui::LocalState::new(self.continuity.allowed(capability)),
                        ContinuityPrivacy::ALLOW_CAPABILITY
                            .map(move |on: bool| (facet.clone(), on)),
                    ),
            );
        }
        privacy_rows.push(note(&t("mesh-privacy-stay")));

        let current_target_label = t("mesh-current-target");
        let source_label = t("mesh-source");
        let mut provenance_rows = vec![
            setting_row_described(
                &current_target_label,
                &target_detail,
                Box::new(Text::new(target_name).numeric().dimmed()),
            ),
            value_row(&source_label, &source_detail),
        ];
        if current_target.is_none() {
            provenance_rows.push(note(&t("mesh-no-active-until")));
        }

        let audit_rows = if self.continuity.audit.is_empty() {
            vec![note(&t("mesh-audit-empty"))]
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
                .chain(std::iter::once(note(&t("mesh-audit-caption"))))
                .collect()
        };
        let mut roaming_rows = self.roaming_provenance.as_ref().map_or_else(
            || vec![note(&t("mesh-roaming-empty"))],
            |provenance| {
                let summaries = provenance_summaries(provenance);
                if summaries.is_empty() {
                    vec![note(&t("mesh-roaming-none"))]
                } else {
                    summaries
                        .into_iter()
                        .map(|(name, detail)| value_row(&name, &detail))
                        .collect()
                }
            },
        );
        if let Some(status) = &self.roaming_provenance_status {
            roaming_rows.push(note(status));
        }
        roaming_rows.push(note(&t("mesh-roaming-caption")));
        let mut home_server_rows = vec![note(&t("mesh-home-caption"))];
        if self.registered_home_peers > 0 {
            let paired = t("mesh-paired-homes");
            home_server_rows.push(value_row(&paired, &self.registered_home_peers.to_string()));
        }
        if pairing || self.load_status.starts_with("Home server") {
            home_server_rows.push(note(&mesh_status_shown(&self.load_status)));
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
        let mut sections: Vec<SettingsSection> = vec![
            card(&continue_title, rows(continuation_rows)),
            card(&privacy_title, rows(privacy_rows)),
            card(&route_title, rows(provenance_rows)),
            card(&roaming_title, rows(roaming_rows)),
            card(&activity_title, rows(audit_rows)),
            card_with_action(&add_personal, approve_personal, rows(personal_rows)),
            card(&trusted_title, rows(enrolled_device_rows)),
            card(&availability_title, rows(availability_rows)),
            card(&synced_title, workspace_rows),
        ];
        sections.extend(self.community_section());
        sections.push(card_with_action(&home_nas, approve, rows(home_server_rows)));
        for peer in &self.home_peers {
            let device = &peer.device_node_id;
            let rename_key = peer_key("name", device);
            let typed = self.peer_name(peer);
            let name = TextInput::local()
                .with_id(&rename_key)
                .with_external_buffer(Arc::clone(&typed))
                .with_placeholder(&peer.label);
            let renamed = device.clone();
            let rename = Box::new(self.cx.bind(
                Button::new(&peer_key("rename", device), &t("mesh-rename")),
                effect(DeviceMeshEffect::RENAME_HOME_SERVER).map(move |()| {
                    (
                        renamed.clone(),
                        typed
                            .lock()
                            .map(|name| name.trim().to_owned())
                            .unwrap_or_default(),
                    )
                }),
            ));
            let remove = Box::new(self.cx.bind_invocation(
                Button::new(&peer_key("remove", device), &t("mesh-revoke-remove")),
                DeviceMeshEffect::RemoveHomeServer {
                    device: device.clone(),
                },
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
            peer_rows.push(note(&t("mesh-files-stay-desc")));
            sections.push(card_with_action(&peer.label, remove, rows(peer_rows)));
        }
        if shows_advanced_services(self.config.device_role) {
            sections.push(card(
                &t("mesh-services-title"),
                rows(vec![
                    self.service_row(
                        "mesh-svc-relay",
                        "mesh-svc-relay-desc",
                        "inbound_relay",
                        "device_mesh.inbound_relay",
                        self.config.services.inbound_relay,
                    ),
                    self.service_row(
                        "mesh-svc-remote",
                        "mesh-svc-remote-desc",
                        "remote_control_relay",
                        "device_mesh.remote_relay",
                        self.config.services.remote_control_relay,
                    ),
                    self.service_row(
                        "mesh-svc-apps",
                        "mesh-svc-apps-desc",
                        "app_serving",
                        "device_mesh.app_serving",
                        self.config.services.app_serving,
                    ),
                    self.service_row(
                        "mesh-svc-jobs",
                        "mesh-svc-jobs-desc",
                        "compute",
                        "device_mesh.compute",
                        self.config.services.compute,
                    ),
                ]),
            ));
        }

        cards(sections)
    }

    /// The row switching the mesh service `service` of this device.
    fn service_row(
        &self,
        label: &str,
        description: &str,
        service: &'static str,
        id: &str,
        on: bool,
    ) -> crate::widgets::Item {
        self.cx
            .row(t(label))
            .described(t(description))
            .id(id)
            .toggle_effect(
                &ui::LocalState::new(on),
                effect(DeviceMeshEffect::SET_DEVICE_SERVICE)
                    .map(move |on: bool| (service.to_owned(), on)),
            )
    }

    fn persist(&mut self) {
        if self.config_path == std::path::Path::new(DEFAULT_CONFIG) {
            if self.apply_child.is_some() {
                self.apply_queued = true;
                // Applying the change is the system's job; only a failure
                // is news.
                self.load_status.clear();
            } else {
                self.start_apply();
            }
            return;
        }
        self.load_status = match self.config.save(&self.config_path) {
            Ok(()) => String::new(),
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
                self.load_status.clear();
            }
            Err(error) => self.load_status = format!("Not saved: {error}"),
        }
    }

    /// Pair a nearby device through the host's actions: the first choice
    /// asks the host to look for one, the next approves the one found. The
    /// host runs discovery and the polkit-gated approval; nothing here
    /// starts a process.
    fn approve_enrollment(&mut self, target: PairingTarget) {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, device_pairing};
        if self.pairing_call.is_some() {
            return;
        }
        let role = match target {
            PairingTarget::HomePeer => "home-peer",
            PairingTarget::PersonalDevice => "personal-device",
        };
        let Some(candidate) = self.pairing_candidate.clone() else {
            self.pairing_target = target;
            let mut arguments = serde_json::Map::new();
            arguments.insert("role".into(), role.into());
            self.pairing_call = Some(self.remote.invoke(
                device_pairing::target(device_pairing::DISCOVER),
                InvokeArgument::Json(arguments),
                None,
            ));
            self.pairing_discovery = true;
            self.load_status = match target {
                PairingTarget::HomePeer => "Looking for your new home server…".into(),
                PairingTarget::PersonalDevice => "Looking for your new device…".into(),
            };
            self.changed = true;
            return;
        };
        if self.pairing_target != target {
            self.pairing_candidate = None;
            self.approve_enrollment(target);
            return;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("role".into(), role.into());
        arguments.insert("device".into(), candidate.device.clone().into());
        arguments.insert("service".into(), candidate.service.clone().into());
        if target == PairingTarget::HomePeer {
            let ids: Vec<u64> = if self.config.workspaces.is_empty() {
                self.discovered_workspaces
                    .iter()
                    .map(|workspace| workspace.id)
                    .collect()
            } else {
                self.config
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id)
                    .collect()
            };
            arguments.insert("workspace_ids".into(), ids.into());
        }
        // Choosing the button that shows the device's code is the person's
        // confirmation that both screens show it.
        arguments.insert("confirm".into(), true.into());
        self.pairing_call = Some(self.remote.invoke(
            device_pairing::target(device_pairing::APPROVE),
            InvokeArgument::Json(arguments),
            Some(Confirmation::Confirmed),
        ));
        self.pairing_discovery = false;
        self.load_status = format!(
            "Approving {} with code {}…",
            candidate.label, candidate.verification_code
        );
        self.changed = true;
    }

    /// Take in the host's answer to the pairing step, once it settled.
    fn poll_pairing(&mut self) {
        use ui::tilo_actions::InvocationUpdate;
        let Some(call) = self.pairing_call.as_ref() else {
            return;
        };
        self.remote.poll();
        let answer = call.get();
        if !answer.is_settled() {
            return;
        }
        self.pairing_call = None;
        let discovery = std::mem::take(&mut self.pairing_discovery);
        self.changed = true;
        let output = match answer {
            ui::RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => output,
            ui::RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => {
                self.load_status = format!(
                    "Device was not paired: {}. Choose the pairing action to retry.",
                    error.message
                );
                return;
            }
            ui::RemoteCall::Done(_) => {
                self.load_status =
                    "Pairing stopped: it was cancelled. Choose the pairing action to retry.".into();
                return;
            }
            ui::RemoteCall::Refused(error) => {
                self.load_status =
                    format!("Pairing stopped: {error}. Choose the pairing action to retry.");
                return;
            }
            ui::RemoteCall::Pending => return,
        };
        if discovery {
            match output
                .value
                .map(serde_json::from_value::<PairCandidate>)
                .transpose()
            {
                Ok(Some(candidate)) => {
                    self.load_status = format!(
                        "Found {}. Confirm code {} on both devices, then approve.",
                        candidate.label, candidate.verification_code
                    );
                    self.pairing_candidate = Some(candidate);
                }
                Ok(None) | Err(_) => {
                    self.load_status = "Nearby discovery returned an invalid identity".into();
                }
            }
            return;
        }
        self.registered_home_peers = registered_home_peer_count();
        self.refresh_runtime_status();
        self.load_status = match self.pairing_target {
            PairingTarget::HomePeer => "Home server paired".into(),
            PairingTarget::PersonalDevice => {
                self.refresh_personal_devices();
                "Personal device added".into()
            }
        };
        self.pairing_candidate = None;
    }

    fn start_home_peer_command(
        &mut self,
        peer: &tilo_relay_protocol::HomePeerRegistration,
        action: &str,
        label: Option<&str>,
    ) {
        if self.management_child.is_some() || self.management_call.is_some() {
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
                    .insert(peer.device_node_id.clone(), format!("Offline: {error}"));
            }
        }
        self.changed = true;
    }

    fn refresh_one_home_peer(&mut self) {
        if self.management_child.is_some() || self.management_call.is_some() {
            return;
        }
        let pending = identity_root().join(PENDING_DECOMMISSIONS);
        self.removals_pending = std::fs::read(&pending)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).ok())
            .is_some_and(|pending| !pending.is_empty());
        if self.removals_pending {
            // Delivering a removal signs with the root key: the host runs it.
            use ui::tilo_actions::remote::{InvokeArgument, identity_admin};
            let call = self.remote.invoke(
                identity_admin::target(identity_admin::HOME_PEER_RETRY),
                InvokeArgument::None,
                None,
            );
            self.management_call = Some((call, "__pending__".into()));
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
                        self.load_status.clear();
                    }
                } else {
                    self.apply_queued = false;
                    self.load_status = format!(
                        "Not saved: {}",
                        command_error(&output.stderr, "administrator approval was not granted")
                    );
                }
                self.changed = true;
            }
            Some(Err(error)) => {
                self.apply_child = None;
                self.apply_queued = false;
                self.load_status = format!("Not saved: {error}");
                self.changed = true;
            }
            None => {}
        }

        self.poll_pairing();
        self.poll_host_calls();

        let management_result = self
            .management_child
            .as_mut()
            .and_then(bounded_command::AsyncCommand::try_wait);
        match management_result {
            Some(Ok(output)) => {
                self.management_child = None;
                let device = self.management_device.take().unwrap_or_default();
                let answer = if output.status.success() {
                    Ok(serde_json::from_slice::<serde_json::Value>(&output.stdout).ok())
                } else {
                    Err(command_error(&output.stderr, "not reachable"))
                };
                self.take_management_answer(device, answer);
            }
            Some(Err(error)) => {
                self.management_child = None;
                if let Some(device) = self.management_device.take() {
                    self.home_peer_health
                        .insert(device, format!("Offline: {error}"));
                }
                self.changed = true;
            }
            None => {}
        }
    }
}

impl PaneModel for DeviceMeshModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_background_tasks();
        if self.continuity_settings.refresh() {
            self.continuity = self.continuity_settings.get().clone();
            self.changed = true;
        }
        if let Some(follow) = self.following.upgrade() {
            self.take_followed(&follow);
        } else if ui::time::now() >= self.next_status_refresh {
            self.refresh_runtime_status();
            self.refresh_one_home_peer();
        }
        let typed: Vec<String> = self
            .home_peers
            .iter()
            .map(|peer| peer.device_node_id.clone())
            .collect();
        self.peer_names.retain(|device, _| typed.contains(device));
        for peer in &self.home_peers {
            self.peer_names
                .entry(peer.device_node_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(peer.label.clone())));
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "DeviceMeshPane"
    }

    fn busy(&self) -> bool {
        self.apply_child.is_some() || self.pairing_call.is_some() || self.management_child.is_some()
    }

    fn next_wake(&self) -> Option<Instant> {
        if !shows_advanced_services(self.config.device_role) {
            return None;
        }
        match self.following.upgrade() {
            // Only home servers' health is asked on a timer, and files that
            // cannot be watched are read with it.
            Some(follow) => (!self.home_peers.is_empty()
                || self.removals_pending
                || !follow.files.is_complete())
            .then_some(self.next_status_refresh),
            None => Some(self.next_status_refresh),
        }
    }

    fn follow(&mut self) -> Option<Rc<dyn std::any::Any>> {
        // Only a serving device shows the relay's status; a pane opened
        // before the role changed reads it on its timer instead.
        if !self.cx.reaches_devices() || !shows_advanced_services(self.config.device_role) {
            return None;
        }
        if let Some(follow) = self.following.upgrade() {
            return Some(follow);
        }
        let files: Vec<PathBuf> = self
            .roaming_provenance_path
            .iter()
            .cloned()
            .chain([identity_root().join(PENDING_DECOMMISSIONS)])
            .collect();
        let follow = Rc::new(MeshFollow::start(&self.config, &files)?);
        self.following = Rc::downgrade(&follow);
        // Ask every home server once on opening, then once a minute.
        self.health_round = self.home_peers.len().max(1);
        self.next_status_refresh = ui::time::now();
        Some(follow)
    }
}

impl DeviceMeshModel {
    /// Take in what the shown pane follows: the relay's status as its
    /// thread read it, the files when their watch says they changed, and
    /// the next home server's health when it is due.
    fn take_followed(&mut self, follow: &MeshFollow) {
        if let Some(status) = follow.status()
            && status != self.runtime_status
        {
            self.runtime_status = status;
            self.changed = true;
        }
        let now = ui::time::now();
        let due = now >= self.next_status_refresh;
        let changed = follow.files.take() || (due && !follow.files.is_complete());
        if changed {
            self.refresh_roaming_provenance();
        }
        let idle = self.management_child.is_none() && self.management_call.is_none();
        if idle && (changed || due || self.health_round > 0) {
            self.health_round = self.health_round.saturating_sub(1);
            self.next_status_refresh = now + HOME_PEER_HEALTH_INTERVAL;
            self.refresh_one_home_peer();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A configuration directory or relay state that is not visible from
    /// here is reported as unreadable, not as an unconfigured or offline
    /// device.
    #[test]
    fn invisible_relay_state_is_unreadable_not_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join("etc-tilo").join("relay.toml");
        let pane = DeviceMeshModel::with_path(&hidden);
        assert!(
            pane.load_status
                .starts_with("Configuration could not be read: "),
            "{}",
            pane.load_status
        );
        let config = RelayConfig {
            state_root: dir.path().join("missing").join("tilo-relay"),
            ..RelayConfig::default()
        };
        assert_eq!(
            RelayRuntimeStatus::read(&config, ui::time::wall()),
            RelayRuntimeStatus::Unreadable
        );
        let config = RelayConfig {
            state_root: dir.path().join("tilo-relay"),
            ..RelayConfig::default()
        };
        assert_eq!(
            RelayRuntimeStatus::read(&config, ui::time::wall()),
            RelayRuntimeStatus::Offline
        );
    }

    /// The status thread follows the relay's heartbeat as it is written
    /// and notices when it stops, so a shown pane wakes for a change of
    /// status rather than for every heartbeat or on a timer.
    #[test]
    fn the_relay_status_follows_its_heartbeat_and_its_lapse() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("tilo-relay");
        std::fs::create_dir_all(&state_root).unwrap();
        let config = RelayConfig {
            full_replica_status: state_root.join("full-replica.json"),
            state_root,
            device_role: DeviceRole::Hybrid,
            ..RelayConfig::default()
        };
        let follow = MeshFollow::start(&config, &[]).expect("the status thread starts");
        tilo_test_env::wait_until("the first read", || {
            follow.status() == Some(RelayRuntimeStatus::Offline)
        });
        let metrics = tilo_relay_protocol::metrics::MetricsSnapshot::default();
        tilo_relay_protocol::metrics::save_metrics(&config.state_root, &metrics).unwrap();
        tilo_test_env::wait_until("the heartbeat to show the relay online", || {
            follow.status() == Some(RelayRuntimeStatus::Online)
        });
        tilo_test_env::wait_until("the lapsed heartbeat to show it offline", || {
            follow.status() == Some(RelayRuntimeStatus::Offline)
        });
    }

    /// A shown pane takes the status its thread read and re-reads its
    /// files on their watch's word; with no home server to ask it sets no
    /// deadline, and a removal still to deliver is retried once a minute.
    #[test]
    fn a_followed_pane_reads_on_a_change_and_sets_no_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", dir.path());
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        pane.config.device_role = DeviceRole::Hybrid;
        let follow = Rc::new(MeshFollow::by_hand());
        pane.following = Rc::downgrade(&follow);
        let _ = pane.poll();
        assert_eq!(pane.next_wake(), None, "an idle pane sets no deadline");

        *follow.status.lock().unwrap() = Some(RelayRuntimeStatus::Online);
        assert!(pane.poll(), "the thread's status is shown");
        assert_eq!(pane.runtime_status, RelayRuntimeStatus::Online);
        assert!(!pane.poll(), "once");

        let provenance = pane.roaming_provenance_path.clone().unwrap();
        std::fs::write(&provenance, "{}").unwrap();
        assert!(!pane.poll(), "nothing is read without the watch's word");
        follow.files.touch();
        assert!(pane.poll(), "the changed provenance is read");

        pane.removals_pending = true;
        assert!(
            pane.next_wake()
                .is_some_and(|at| at >= ui::time::now() + Duration::from_secs(30)),
            "a pending removal is retried once a minute, not every two seconds"
        );
    }

    /// Continuity controls, conflict summaries, refused future provenance and redacted audit rows.
    #[test]
    fn continuity_controls_are_typed_audited_and_refuse_future_provenance() {
        // Case `continuity_ui_exposes_master_and_every_typed_capability_control`.
        {
            let dir = tempfile::tempdir().unwrap();
            let pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
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
                    key: "tilo.appearance.mode".into(),
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
                        "tilo.appearance.mode".into(),
                        "Resolved to Laptop at revision 8; differs from Desktop".into()
                    ),
                    (
                        "Wi-Fi: Home".into(),
                        "Sealed WPA2 profile. From Laptop at revision 9".into()
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
            use crate::host::testing::{CONTINUITY, invoke_section};
            let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
            invoke_section(
                &mut app,
                CONTINUITY,
                "allow-continuity",
                serde_json::json!({ "on": true }),
            )
            .unwrap();
            invoke_section(
                &mut app,
                CONTINUITY,
                "allow-capability",
                serde_json::json!({ "capability": "clipboard", "on": true }),
            )
            .unwrap();
            assert!(
                invoke_section(
                    &mut app,
                    CONTINUITY,
                    "allow-capability",
                    serde_json::json!({ "capability": "telepathy", "on": true }),
                )
                .is_err()
            );

            let saved = app.settings::<ContinuityPrivacy>().get();
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

            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            pane.apply(&DeviceMeshEffect::SetDeviceRole {
                role: ROLE_ACTIVE.into(),
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetReplication {
                replication: REPLICATION_FULL.into(),
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetDeviceService {
                service: "remote_control_relay".into(),
                on: false,
            })
            .unwrap();

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
            let mut pane = DeviceMeshModel::with_path(&path);
            assert!(pane.load_status.contains("Not configured"));
            assert_eq!(pane.config.device_role, DeviceRole::Interactive);
            assert_eq!(
                pane.config.services,
                tilo_relay_protocol::ServiceCapabilities::default()
            );
            let _ = pane.poll();
            assert!(!path.exists());
            pane.apply(&DeviceMeshEffect::SetDeviceRole {
                role: ROLE_ACTIVE.into(),
            })
            .unwrap();
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
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
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
            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            assert!(!pane.poll());

            pane.apply(&DeviceMeshEffect::SetDeviceRole {
                role: ROLE_ACTIVE.into(),
            })
            .unwrap();

            assert!(pane.poll());
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

            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            pane.apply(&DeviceMeshEffect::SetDeviceRole {
                role: ROLE_SERVER.into(),
            })
            .unwrap();

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
            assert_eq!(peer_health_badge("Online. Files ready"), "Online");
            assert_eq!(peer_health_badge("Online. Syncing files"), "Syncing");
            assert_eq!(peer_health_badge("Offline: Last seen 4 h ago"), "Offline");
            assert_eq!(peer_health_badge("Checking…"), "Checking");
        }

        // Case `discovered_workspace_toggle_updates_the_real_binding_set`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut pane = DeviceMeshModel::with_path(&path);
            pane.discovered_workspaces = vec![DiscoveredWorkspace {
                id: 44,
                name: "Studio".into(),
            }];
            pane.apply(&DeviceMeshEffect::SetSyncedWorkspace {
                workspace: 44,
                on: true,
            })
            .unwrap();
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

    /// Removing a home server and revoking a device ask first, and only
    /// what is paired is touched; pairing is single-flight.
    #[test]
    fn device_removal_asks_first_and_pairing_is_single_flight() {
        // Case `removal_and_revocation_ask_first_and_refuse_strangers`.
        {
            for effect in [
                DeviceMeshEffect::RemoveHomeServer {
                    device: "11".repeat(32),
                },
                DeviceMeshEffect::RevokeDevice {
                    device: "2a".repeat(32),
                    reason: "Retired".into(),
                },
            ] {
                let declaration = DeviceMeshEffect::declarations()
                    .into_iter()
                    .find(|declaration| declaration.id == effect.id())
                    .unwrap();
                assert_eq!(
                    declaration.risk,
                    ui::tilo_actions::RiskLevel::Destructive,
                    "{} asks first",
                    effect.id()
                );
                let dir = tempfile::tempdir().unwrap();
                let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
                assert!(pane.apply(&effect).is_err(), "nothing unpaired is touched");
                assert!(pane.management_child.is_none());
            }
        }

        // Case `duplicate_pair_action_is_ignored_while_pairing_is_active`.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
            let host = Arc::new(PairingHost::default());
            pane.remote = ui::RemoteActions::with_host(
                host.clone(),
                ui::tilo_actions::remote::Caller::local_person("owner"),
            );
            pane.approve_enrollment(PairingTarget::HomePeer);
            pane.approve_enrollment(PairingTarget::HomePeer);
            assert_eq!(host.asked().len(), 1, "one pairing step at a time");
        }
    }

    const NEARBY_DEVICE: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const THIS_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const PHONE: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const NEARBY_SERVICE: &str = "bb00000000000000000000000000000000000000000000000000000000000002";

    /// The host's pairing actions, in process: it records what it was asked
    /// and answers the way the host does.
    #[derive(Default)]
    struct PairingHost {
        asked: Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
    }

    impl PairingHost {
        fn asked(&self) -> Vec<ui::tilo_actions::remote::InvokeRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl ui::tilo_actions::remote::ActionHost for PairingHost {
        fn query(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: ui::tilo_actions::remote::CatalogueQuery,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::CataloguePage,
                ui::tilo_actions::remote::ActionHostError,
            >,
        > {
            ui::tilo_actions::remote::Pending::ready(Err(
                ui::tilo_actions::remote::ActionHostError::NotPermitted,
            ))
        }

        fn invoke(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            request: ui::tilo_actions::remote::InvokeRequest,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::InvocationReceipt,
                ui::tilo_actions::remote::ActionHostError,
            >,
        > {
            use ui::tilo_actions::remote::{community, device_pairing, identity_admin};
            let invocation = request.invocation;
            let value = if request.target.provider == community::PROVIDER {
                // In the workspace unless the last change asked was a leave.
                let last_change = self
                    .asked()
                    .into_iter()
                    .rev()
                    .map(|asked| asked.target.address.action)
                    .find(|step| step == community::JOIN || step == community::LEAVE);
                let step = request.target.address.action.as_str();
                let left = match step {
                    community::STATUS => last_change.as_deref() == Some(community::LEAVE),
                    _ => step == community::LEAVE,
                };
                serde_json::json!({"site": "https://tilo.coffee/", "joined": !left, "left": left})
            } else if request.target.provider == identity_admin::PROVIDER {
                match request.target.address.action.as_str() {
                    identity_admin::DEVICES => serde_json::json!([
                        {
                            "label": "This laptop", "key_hex": THIS_DEVICE,
                            "fingerprint": "11 11 11 11 11 11", "current": true,
                            "revoked": false, "can_host": true,
                        },
                        {
                            "label": "Phone", "key_hex": PHONE,
                            "fingerprint": "22 22 22 22 22 22", "current": false,
                            "revoked": false, "can_host": false,
                        },
                    ]),
                    identity_admin::HOME_PEER_REMOVE => {
                        serde_json::json!({"decommissioned": true, "files_ready": false})
                    }
                    _ => serde_json::json!({"device": PHONE, "revoked": true}),
                }
            } else if request.target.address.action == device_pairing::DISCOVER {
                serde_json::json!({
                    "role": "home-peer",
                    "device": NEARBY_DEVICE,
                    "service": NEARBY_SERVICE,
                    "label": "Kitchen NAS",
                    "verification_code": "123-456",
                })
            } else {
                serde_json::json!({"role": "home-peer", "device": NEARBY_DEVICE, "label": "Kitchen NAS"})
            };
            self.asked.lock().unwrap().push(request);
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::InvocationReceipt {
                    invocation,
                    status: ui::tilo_actions::remote::ReceiptStatus::Completed {
                        output: ui::tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                },
            ))
        }

        fn observe(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Option<ui::tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Result<(), ui::tilo_actions::remote::ActionHostError> {
            Ok(())
        }
    }

    /// The device fleet and the root key are the host's: the pane lists the
    /// devices and revokes one through its identity actions, with the
    /// person's confirmation, and reads no fleet file.
    #[test]
    fn device_revocation_runs_on_the_host() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        let host = Arc::new(PairingHost::default());
        pane.remote = ui::RemoteActions::with_host(
            host.clone(),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );

        pane.refresh_personal_devices();
        pane.poll_background_tasks();
        assert_eq!(
            pane.personal_devices
                .iter()
                .map(|device| (device.label.as_str(), device.current))
                .collect::<Vec<_>>(),
            [("This laptop", true), ("Phone", false)]
        );
        assert!(
            pane.apply(&DeviceMeshEffect::RevokeDevice {
                device: THIS_DEVICE.into(),
                reason: "Retired".into(),
            })
            .is_err(),
            "the device in use is revoked from another one"
        );

        pane.apply(&DeviceMeshEffect::RevokeDevice {
            device: PHONE.into(),
            reason: "Lost".into(),
        })
        .unwrap();
        pane.poll_background_tasks();
        assert_eq!(
            pane.load_status,
            "Phone can no longer access your workspaces"
        );
        let asked = host.asked();
        let revoke = &asked[1];
        assert_eq!(
            revoke.target,
            identity_admin::target(identity_admin::REVOKE)
        );
        assert_eq!(revoke.confirmation, Some(Confirmation::Confirmed));
        assert_eq!(
            revoke.argument,
            InvokeArgument::Json(
                serde_json::json!({"device": PHONE, "reason": "lost", "confirm": true})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(
            asked[2].target,
            identity_admin::target(identity_admin::DEVICES),
            "the list is read again after the revocation"
        );
    }

    /// The community workspace is the host's: the pane reads where the
    /// person stands, leaves with their confirmation and joins again, and
    /// reads the status again after each.
    #[test]
    fn community_workspace_is_joined_and_left_on_the_host() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, community};
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        assert!(
            pane.apply(&DeviceMeshEffect::LeaveCommunity).is_err(),
            "nothing is left before the host said there is a community workspace"
        );
        let host = Arc::new(PairingHost::default());
        pane.remote = ui::RemoteActions::with_host(
            host.clone(),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );

        pane.refresh_community();
        pane.poll_background_tasks();
        let joined = CommunityStatus {
            site: Some("https://tilo.coffee/".into()),
            joined: true,
            left: false,
        };
        assert_eq!(pane.community.as_ref(), Some(&joined));
        assert!(
            pane.apply(&DeviceMeshEffect::JoinCommunity).is_err(),
            "a computer in the workspace does not join it twice"
        );

        pane.apply(&DeviceMeshEffect::LeaveCommunity).unwrap();
        assert!(pane.community_change.is_some());
        pane.poll_background_tasks();
        pane.poll_background_tasks();
        assert_eq!(
            pane.community
                .as_ref()
                .map(|status| (status.joined, status.left)),
            Some((false, true))
        );
        assert!(pane.community_error.is_none());

        pane.apply(&DeviceMeshEffect::JoinCommunity).unwrap();
        pane.poll_background_tasks();
        pane.poll_background_tasks();
        assert_eq!(pane.community.as_ref(), Some(&joined));

        let asked: Vec<_> = host
            .asked()
            .into_iter()
            .filter(|request| request.target.provider == community::PROVIDER)
            .collect();
        assert_eq!(
            asked
                .iter()
                .map(|request| request.target.address.action.as_str())
                .collect::<Vec<_>>(),
            [
                community::STATUS,
                community::LEAVE,
                community::STATUS,
                community::JOIN,
                community::STATUS,
            ],
            "the status is read again after each change"
        );
        assert_eq!(asked[1].confirmation, Some(Confirmation::Confirmed));
        assert_eq!(
            asked[1].argument,
            InvokeArgument::Json(
                serde_json::json!({"confirm": true})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(asked[3].argument, InvokeArgument::None);
    }

    /// Removing a home server signs its revocation with the root key, so
    /// the host runs it: the pane invokes the host's removal with the
    /// person's confirmation and runs no relay process.
    #[test]
    fn home_server_removal_runs_on_the_host() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        let dir = tempfile::tempdir().unwrap();
        // A removed server's last sighting is written below the identity
        // root: the fixture's, never the developer's.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", dir.path());
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        let host = Arc::new(PairingHost::default());
        pane.remote = ui::RemoteActions::with_host(
            host.clone(),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );
        let server: tilo_relay_protocol::HomePeerRegistration =
            serde_json::from_value(serde_json::json!({
                "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                "device_node_id": NEARBY_DEVICE,
                "relay_service_node_id": NEARBY_SERVICE,
                "label": "Kitchen NAS",
                "user_public_key": "33".repeat(32),
                "relay_workspace_ids": [],
            }))
            .unwrap();
        pane.home_peers = vec![server];
        pane.apply(&DeviceMeshEffect::RemoveHomeServer {
            device: NEARBY_DEVICE.into(),
        })
        .unwrap();
        assert!(
            pane.management_child.is_none(),
            "no relay process runs here"
        );
        pane.poll_background_tasks();
        let asked = host.asked();
        let removal = asked.last().unwrap();
        assert_eq!(
            removal.target,
            identity_admin::target(identity_admin::HOME_PEER_REMOVE)
        );
        assert_eq!(removal.confirmation, Some(Confirmation::Confirmed));
        assert_eq!(
            removal.argument,
            InvokeArgument::Json(
                serde_json::json!({
                    "device": NEARBY_DEVICE,
                    "service": NEARBY_SERVICE,
                    "confirm": true,
                })
                .as_object()
                .unwrap()
                .clone()
            )
        );
        assert_eq!(
            pane.home_peer_health.get(NEARBY_DEVICE).map(String::as_str),
            Some("Removed")
        );
        assert!(pane.management_call.is_none(), "the removal has answered");
    }

    /// Pairing runs on the host behind its actions: discovering asks the
    /// host, approving invokes its approval with the device found and the
    /// person's confirmation, and the pane starts no process of its own.
    #[test]
    fn pairing_invokes_the_host_actions_and_spawns_nothing() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, device_pairing};
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        pane.config.workspaces = vec![WorkspaceBinding {
            id: 7,
            pin: true,
            replication: ReplicationIntent::OnDemand,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: CheckpointPrefixRecovery::default(),
        }];
        let host = Arc::new(PairingHost::default());
        pane.remote = ui::RemoteActions::with_host(
            host.clone(),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );

        pane.apply(&DeviceMeshEffect::PairHomeServer).unwrap();
        pane.poll_background_tasks();
        let asked = host.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].target,
            device_pairing::target(device_pairing::DISCOVER)
        );
        assert_eq!(
            asked[0].argument,
            InvokeArgument::Json(
                serde_json::json!({"role": "home-peer"})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(
            pane.load_status,
            "Found Kitchen NAS. Confirm code 123-456 on both devices, then approve."
        );
        assert!(pane.pairing_call.is_none(), "discovery has answered");

        pane.apply(&DeviceMeshEffect::PairHomeServer).unwrap();
        pane.poll_background_tasks();
        let asked = host.asked();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            asked[1].target,
            device_pairing::target(device_pairing::APPROVE)
        );
        assert_eq!(
            asked[1].argument,
            InvokeArgument::Json(
                serde_json::json!({
                    "role": "home-peer",
                    "device": NEARBY_DEVICE,
                    "service": NEARBY_SERVICE,
                    "workspace_ids": [7],
                    "confirm": true,
                })
                .as_object()
                .unwrap()
                .clone()
            )
        );
        assert_eq!(asked[1].confirmation, Some(Confirmation::Confirmed));
        assert_eq!(pane.load_status, "Home server paired");
        assert!(
            pane.pairing_candidate.is_none(),
            "the paired device is done"
        );
        assert!(
            pane.apply_child.is_none() && pane.management_child.is_none(),
            "pairing started no process in the app"
        );
    }

    /// Wait for a background read to end, as the warm-up's wake would.
    fn until_finished(finished: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !finished() {
            assert!(std::time::Instant::now() < deadline, "the read ends");
            std::thread::yield_now();
        }
    }

    /// The warm-up reads the relay and the fleet off the UI thread once;
    /// the model is built from that read, and nothing is read again.
    #[test]
    fn the_area_builds_its_model_from_the_read_it_started() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = DeviceMeshArea::new(&cx);
        assert!(area.start_read(), "the first read starts");
        assert!(!area.start_read(), "one read at a time");
        assert!(!area.is_open(), "a running read publishes nothing yet");
        until_finished(|| area.read_finished());
        let model = area.model().borrow();
        assert!(area.is_open());
        assert_eq!(
            model.config_path,
            PathBuf::from("/nonexistent/tilo/relay.toml"),
            "a test reads the stand-in configuration"
        );
        assert!(model.personal_devices.is_empty() && model.home_peers.is_empty());
        drop(model);
        assert!(!area.start_read(), "a built model is not read again");
    }

    /// A model built from a read shows what reading in place shows.
    #[test]
    fn a_read_builds_the_model_reading_in_place_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        std::fs::write(&path, "device_role = \"interactive\"\n").unwrap();
        let cx = PaneCx::in_file(dir.path().join("settings.json"));
        let here = DeviceMeshModel::from_path(cx.clone(), path.clone(), false);
        let read = MeshRead::of(path, false, ui::time::wall());
        let there = DeviceMeshModel::from_read(cx, read);
        assert_eq!(here.config_path, there.config_path);
        assert_eq!(here.load_status, there.load_status);
        assert_eq!(here.config.device_role, there.config.device_role);
        assert_eq!(here.runtime_status, there.runtime_status);
        assert_eq!(here.discovered_workspaces, there.discovered_workspaces);
    }
}
