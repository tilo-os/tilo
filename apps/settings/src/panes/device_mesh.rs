//! Sync between devices — configure seamless continuation and always-on sync.
//! The pane reads and writes the same `RelayConfig` consumed by `tilo-relay`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::time::{Duration, Instant, SystemTime};

use provider_settings_ui::bound::SettingRows as _;
use tilo_relay_protocol::{RelayConfig, WorkFrom, WorkSetting};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource::{self, Effect};
use ui::widget::{DisclosureGroup, Element};

use crate::area::AreaContext;
use crate::continuity::{self, Capability, ContinuityPrivacy};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    SettingsSection, StatusTone, error_caption, note, page_of, row, rows, section, value_row,
};
use settings_store::roaming_provenance::{Origin, RoamingProvenance};

mod devices;
mod home_relay;
#[cfg(test)]
mod home_relay_tests;

crate::effects::effect_family!(DeviceMeshEffect => answer);

crate::area::pane_area! {
    DeviceMeshArea;
    fn poll(&self) {
        DeviceMeshArea::poll(self);
    }
    fn next_wake(&self) -> Option<Instant> {
        DeviceMeshArea::next_wake(self)
    }
    fn busy(&self) -> bool {
        DeviceMeshArea::busy(self)
    }
    fn take_collab_requests(&self) -> Vec<ui::collaboration::CollabRequest> {
        ui::widget::Collaborative::take_collab_requests(&mut self.remote())
    }
    fn handle_collab_event(&self, event: &ui::collaboration::CollabEvent) {
        ui::widget::Collaborative::handle_collab_event(&mut self.remote(), event);
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&DeviceMeshArea> {
    app.areas().find()
}

const DEFAULT_CONFIG: &str = "/etc/tilo/relay.toml";
const CONTINUITY_ENABLED_KEY: &str = "device_mesh.continuity.enabled";
const AGENTS_ACCEPT_KEY: &str = "device_mesh.agents.accept_own_devices";
const CONTROL_HELPER: &str = "/usr/libexec/tilo-home-peer-control";
const RELAY_ADMIN: &str = "/usr/libexec/tilo/tilo-relay-admin";

const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;
const APPLY_TIMEOUT: Duration = Duration::from_secs(120);
const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(60);
const WORK_NOTHING: &str = "Nothing";
const WORK_OWN: &str = "My own devices' work";
const WORK_ANYONE: &str = "Anyone's work in my workspaces";

/// The work this device takes, as a person states it.
fn work_shown(work: &WorkSetting) -> String {
    t(match &work.from {
        WorkFrom::Nobody => "mesh-work-nothing",
        WorkFrom::OwnDevices => "mesh-work-own",
        WorkFrom::Anyone => "mesh-work-anyone",
        WorkFrom::Workspaces(_) => "mesh-work-some",
    })
}

fn work_from_label(label: &str) -> Option<WorkFrom> {
    if label == WORK_NOTHING || label == t("mesh-work-nothing") {
        Some(WorkFrom::Nobody)
    } else if label == WORK_OWN || label == t("mesh-work-own") {
        Some(WorkFrom::OwnDevices)
    } else if label == WORK_ANYONE || label == t("mesh-work-anyone") {
        Some(WorkFrom::Anyone)
    } else {
        None
    }
}

/// The work setting a person picked: every kind of work when it takes any, and none when it takes
/// nobody's.
fn work_from(from: WorkFrom) -> WorkSetting {
    WorkSetting {
        kinds: if from == WorkFrom::Nobody {
            tilo_relay_protocol::WorkKinds::NONE
        } else {
            tilo_relay_protocol::WorkKinds::ALL
        },
        from,
        ..WorkSetting::default()
    }
}

fn takes_work(work: &WorkSetting) -> bool {
    !work.takes_nothing()
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
            Ok(Some(_)) if config.services.full_content_replication => {
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

const HOME_PEER_HEALTH_INTERVAL: Duration = Duration::from_secs(60);

const PENDING_DECOMMISSIONS: &str = "pending-home-peer-decommissions.json";

/// The wall clock the relay-status thread judges the heartbeat by: the
/// process's, unless a test hands it one that reads fresh however long the
/// thread takes to look.
type StatusClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// What a shown Device mesh pane follows: the relay's status, read on a
/// thread of its own, and the files the pane shows.
///
/// The relay rewrites its metrics every second as a heartbeat, so a watch
/// that woke the window for each would be a one-second timer again. The
/// thread re-reads the status on each heartbeat and once its freshness
/// would lapse, and wakes the window only when the status changes.
struct MeshFollow {
    status: Arc<Mutex<Option<RelayRuntimeStatus>>>,
    files: crate::watch::Watched,
    stop: Arc<std::sync::atomic::AtomicBool>,
    poke: std::sync::mpsc::Sender<()>,
}

impl MeshFollow {
    fn start(config: &RelayConfig, files: &[PathBuf]) -> Option<Self> {
        Self::start_at(config, files, Arc::new(ui::time::wall))
    }

    fn start_at(config: &RelayConfig, files: &[PathBuf], now: StatusClock) -> Option<Self> {
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
            Box::new(move || follow_relay(&config, &latest, &stop, poke, &events, &wake, &*now))
        };
        crate::device_read::thread("settings-relay-status", follow).ok()?;
        Some(Self {
            status,
            files: crate::watch::Watched::files(files),
            stop,
            poke,
        })
    }

    #[cfg(test)]
    fn by_hand() -> Self {
        Self {
            status: Arc::new(Mutex::new(None)),
            files: crate::watch::Watched::by_hand(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            poke: std::sync::mpsc::channel().0,
        }
    }

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

fn follow_relay(
    config: &RelayConfig,
    latest: &Mutex<Option<RelayRuntimeStatus>>,
    stop: &std::sync::atomic::AtomicBool,
    poke: std::sync::mpsc::Sender<()>,
    events: &std::sync::mpsc::Receiver<()>,
    wake: &ui::WakeHandle,
    now: &(dyn Fn() -> SystemTime + Send + Sync),
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
        let status = RelayRuntimeStatus::read(config, now());
        if latest
            .lock()
            .is_ok_and(|mut latest| latest.replace(status) != Some(status))
        {
            wake.wake();
        }
        let wait = if !watch.is_complete() {
            Some(STATUS_REFRESH_INTERVAL)
        } else if matches!(
            status,
            RelayRuntimeStatus::Offline | RelayRuntimeStatus::Unreadable
        ) {
            None
        } else {
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

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DeviceMeshEffect {
    /// The work this device takes from the person's other devices. A device
    /// that takes work runs services, so this asks first.
    #[effect(destructive, idempotent)]
    SetWork {
        #[param("Nothing, My own devices' work, or Anyone's work in my workspaces", choices = WORK_FROM)]
        work: String,
    },
    /// Offer or stop one service of a device that takes work.
    #[effect(idempotent)]
    SetDeviceService {
        #[param("full_content_replication, inbound_relay, remote_control_relay, app_serving or compute", choices = SERVICES)]
        service: String,
        on: bool,
    },
    /// Look for a new home server nearby, then approve the one found.
    #[effect(label = "mesh-pair-home")]
    PairHomeServer,
    AddPersonalDevice,
    /// Rename a home server.
    #[effect(row)]
    RenameHomeServer {
        #[param("The home server's node id")]
        device: String,
        #[param("Its new name")]
        name: String,
    },
    /// Remove a home server; it stops serving the person's workspaces.
    #[effect(row, destructive, label = "mesh-remove-home-confirm")]
    RemoveHomeServer {
        #[param("The home server's node id")]
        device: String,
    },
    /// Have the person's home relay host a workspace this device hosts as
    /// well, or no longer.
    #[effect(row, idempotent)]
    SetHomeRelayHosting {
        // The workspace travels under the key the host's step already reads.
        #[param("The workspace's id", name = "room")]
        workspace: String,
        on: bool,
    },
    /// Ask, under a device's row, whether to remove it.
    #[effect(view, row)]
    ReviewRemoval {
        #[param("The device's key or home server's node id")]
        device: String,
    },
    /// Keep the device whose removal was asked about.
    #[effect(view)]
    CancelRemoval,
}

const WORK_FROM: [&str; 3] = [WORK_NOTHING, WORK_OWN, WORK_ANYONE];
const SERVICES: [&str; 5] = [
    "full_content_replication",
    "inbound_relay",
    "remote_control_relay",
    "app_serving",
    "compute",
];

#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DeviceMeshEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let model = crate::panes::device_mesh::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = model.borrow_mut();
    if let DeviceMeshEffect::SetHomeRelayHosting { workspace, on } = &effect {
        model.choose_home_relay(workspace, *on, Some(cx.reply()))?;
        model.changed = true;
        return Ok(Effected::Pending);
    }
    model.apply(&effect).map(|()| Effected::done())
}

pub struct DeviceMeshArea {
    devices: bool,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    model: std::cell::OnceCell<Rc<RefCell<DeviceMeshModel>>>,
    pub(crate) read: crate::device_read::DeviceRead<MeshRead>,
    remote: ui::RemoteActions,
    shown: LocalState<u64>,
}

impl DeviceMeshArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            devices: cx.devices,
            scene: cx.scene.clone(),
            model: std::cell::OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
            remote: ui::RemoteActions::current(),
            shown: LocalState::new(0),
        }
    }

    #[cfg(test)]
    fn over(cx: &AreaContext, model: DeviceMeshModel) -> Self {
        let area = Self::new(cx);
        let _ = area.model.set(Rc::new(RefCell::new(model)));
        area
    }

    pub(crate) fn remote(&self) -> ui::RemoteActions {
        self.remote.clone()
    }

    fn source(&self) -> (PathBuf, bool) {
        if self.devices {
            (PathBuf::from(DEFAULT_CONFIG), true)
        } else {
            (PathBuf::from("/nonexistent/tilo/relay.toml"), false)
        }
    }

    pub fn model(&self) -> &Rc<RefCell<DeviceMeshModel>> {
        self.model.get_or_init(|| {
            let read = self.read.take().unwrap_or_else(|| {
                let (path, devices) = self.source();
                MeshRead::of(path, devices, ui::time::wall())
            });
            let mut model = DeviceMeshModel::from_read(self.devices, read);
            model.remote = self.remote.clone();
            if self.devices {
                model.refresh_personal_devices();
                model.refresh_home_relay();
            } else if let Some(scene) = self.scene.as_deref() {
                model.home_relay.hosting.clone_from(&scene.home_relay);
                match crate::device_fleet::views(scene.fleet_devices.clone()) {
                    Ok(devices) => model.personal_devices = devices,
                    Err(error) => {
                        tracing::warn!(
                            target: "tilo::settings",
                            %error,
                            "the capture's device list is not readable"
                        );
                        model.personal_devices_unavailable = true;
                    }
                }
            }
            Rc::new(RefCell::new(model))
        })
    }

    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let (path, devices) = self.source();
        let now = ui::time::wall();
        self.read.start(move || MeshRead::of(path, devices, now))
    }

    pub(crate) fn poll(&self) {
        let Some(model) = self.model.get() else {
            return;
        };
        let Ok(mut model) = model.try_borrow_mut() else {
            return;
        };
        if !model.shown() {
            model.poll_background_tasks();
            return;
        }
        if model.poll() {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let model = self.model.get()?.try_borrow().ok()?;
        model.shown().then(|| model.next_wake()).flatten()
    }

    fn busy(&self) -> bool {
        self.model
            .get()
            .and_then(|model| model.try_borrow().ok())
            .is_some_and(|model| model.shown() && model.busy())
    }

    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        model
            .home_peers
            .iter()
            .map(home_server_row)
            .chain(model.home_relay_rows())
            .collect()
    }
}

fn home_server_row_id(node: &str) -> String {
    format!("settings.home-server:{node}")
}

fn home_server_row(peer: &tilo_relay_protocol::HomePeerRegistration) -> LiveRow {
    let device = peer.device_node_id.clone();
    LiveRow::new(
        home_server_row_id(&device),
        "settings.home-server",
        peer.label.clone(),
    )
    .operation(
        SettingsEffect::from(DeviceMeshEffect::RenameHomeServer {
            device: device.clone(),
            name: peer.label.clone(),
        })
        .bound_asking(&["name"]),
    )
    .operation(
        SettingsEffect::from(DeviceMeshEffect::ReviewRemoval {
            device: device.clone(),
        })
        .bound(),
    )
    .operation(SettingsEffect::from(DeviceMeshEffect::RemoveHomeServer { device }).bound())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HomePeerHealth {
    Connecting,
    Online { files_ready: bool },
    Removed,
    Unreachable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum MeshStatus {
    #[default]
    Ready,
    NotConfigured,
    ConfigUnreadable,
    NotSaved,
    Looking {
        target: PairingTarget,
    },
    Found {
        label: String,
        code: String,
    },
    Approving {
        label: String,
        code: String,
    },
    PairingStopped {
        cancelled: bool,
    },
    DiscoveryUnusable,
    HomeServerPaired,
    DeviceAdded,
}

impl MeshStatus {
    fn pairing_shown(&self) -> Option<String> {
        Some(match self {
            Self::Looking {
                target: PairingTarget::HomePeer,
            } => t("mesh-status-looking-home"),
            Self::Looking {
                target: PairingTarget::PersonalDevice,
            } => t("mesh-status-looking-device"),
            Self::Found { label, code } => t_args(
                "mesh-status-found",
                &[
                    ("label", label.as_str().into()),
                    ("code", code.as_str().into()),
                ],
            ),
            Self::Approving { label, code } => t_args(
                "mesh-status-approving",
                &[
                    ("label", label.as_str().into()),
                    ("code", code.as_str().into()),
                ],
            ),
            Self::PairingStopped { cancelled: true } => t("mesh-status-pairing-cancelled"),
            Self::PairingStopped { cancelled: false } => t("mesh-status-pairing-failed"),
            Self::DiscoveryUnusable => t("mesh-status-discovery-unusable"),
            Self::HomeServerPaired => t("mesh-status-home-paired"),
            Self::DeviceAdded => t("mesh-status-device-added"),
            Self::Ready | Self::NotConfigured | Self::ConfigUnreadable | Self::NotSaved => {
                return None;
            }
        })
    }

    fn settings_trouble_shown(&self) -> Option<String> {
        match self {
            Self::ConfigUnreadable => Some(t("mesh-status-config-unreadable")),
            Self::NotSaved => Some(t("mesh-status-not-saved")),
            _ => None,
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: what is being saved, paired and delivered, and whether it changed"
)]
pub struct DeviceMeshModel {
    config_path: PathBuf,
    config: RelayConfig,
    status: MeshStatus,
    devices: bool,
    changed: bool,
    runtime_status: RelayRuntimeStatus,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
    home_peer_health: HashMap<String, HomePeerHealth>,
    home_peer_last_seen: HashMap<String, u64>,
    personal_devices: Vec<crate::device_fleet::DeviceView>,
    personal_devices_unavailable: bool,
    /// The session's reading of the person's devices, which says which of them it hears on this
    /// device's local network.
    session_devices: Option<ui::collaboration::SessionResource>,
    /// The person's devices reachable without a relay, by key in lowercase hex.
    nearby: std::collections::BTreeSet<String>,
    device_rows: DisclosureGroup,
    removing: Option<String>,
    roaming_provenance_path: Option<PathBuf>,
    roaming_provenance_stamp: Option<SystemTime>,
    roaming_provenance: Option<RoamingProvenance>,
    roaming_provenance_unreadable: bool,
    management_child: Option<bounded_command::AsyncCommand>,
    management_device: Option<String>,
    management_call: Option<(ui::InvocationCall, Option<String>)>,
    devices_call: Option<ui::InvocationCall>,
    next_peer_health: usize,
    apply_child: Option<bounded_command::AsyncCommand>,
    apply_queued: bool,
    pairing_call: Option<ui::InvocationCall>,
    pairing_discovery: bool,
    pairing_candidate: Option<PairCandidate>,
    pairing_target: PairingTarget,
    remote: ui::RemoteActions,
    next_status_refresh: Instant,
    following: std::rc::Weak<MeshFollow>,
    pages: std::rc::Weak<()>,
    health_round: usize,
    home_relay: home_relay::HomeRelay,
    removals_pending: bool,
}

#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
struct PairCandidate {
    device: String,
    service: String,
    label: String,
    verification_code: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PairingTarget {
    #[default]
    HomePeer,
    PersonalDevice,
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

fn personal_device_key(action: &str, device: &str) -> String {
    format!("device_mesh.personal_device.{action}.{device}.edit")
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

type ProvenanceRead = (Option<RoamingProvenance>, bool, Option<SystemTime>);

fn read_roaming_provenance(path: Option<&std::path::Path>) -> ProvenanceRead {
    let Some(path) = path else {
        return (None, false, None);
    };
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if stamp.is_none() {
        return (None, false, None);
    }
    match RoamingProvenance::load_at(path) {
        Ok(provenance) => (Some(provenance), false, stamp),
        Err(error) => {
            tracing::warn!(
                target: "tilo::settings",
                %error,
                path = %path.display(),
                "the roaming settings could not be read"
            );
            (None, true, stamp)
        }
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

fn peer_health_badge(health: HomePeerHealth) -> String {
    t(match health {
        HomePeerHealth::Connecting => "mesh-badge-checking",
        HomePeerHealth::Online { files_ready: true } => "mesh-badge-online",
        HomePeerHealth::Online { files_ready: false } => "mesh-badge-syncing",
        HomePeerHealth::Removed => "mesh-badge-removed",
        HomePeerHealth::Unreachable => "mesh-badge-offline",
    })
}

fn peer_health_detail(health: HomePeerHealth) -> String {
    t(match health {
        HomePeerHealth::Connecting => "mesh-health-connecting",
        HomePeerHealth::Online { files_ready: true } => "mesh-health-ready",
        HomePeerHealth::Online { files_ready: false } => "mesh-health-syncing",
        HomePeerHealth::Removed => "mesh-badge-removed",
        HomePeerHealth::Unreachable => "mesh-health-unreachable",
    })
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

pub(crate) struct MeshRead {
    config_path: PathBuf,
    config: RelayConfig,
    status: MeshStatus,
    runtime_status: RelayRuntimeStatus,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
    home_peer_last_seen: HashMap<String, u64>,
    roaming_provenance_path: Option<PathBuf>,
    roaming_provenance: ProvenanceRead,
    session_devices: Option<ui::collaboration::SessionResource>,
}

/// The session's reading of the person's devices, as far as this pane reads it.
const SESSION_DEVICES: &str = "session:devices";

#[derive(serde::Deserialize)]
struct SessionDevices {
    #[serde(default)]
    devices: Vec<SessionDevice>,
}

#[derive(serde::Deserialize)]
struct SessionDevice {
    id: String,
    #[serde(default)]
    nearby: bool,
}

/// The devices `reading` says are nearby, by key in lowercase hex.
fn nearby_in(reading: &str) -> Option<std::collections::BTreeSet<String>> {
    let reading: SessionDevices = serde_json::from_str(reading).ok()?;
    Some(
        reading
            .devices
            .into_iter()
            .filter(|device| device.nearby)
            .map(|device| device.id.to_ascii_lowercase())
            .collect(),
    )
}

impl MeshRead {
    fn of(config_path: PathBuf, devices: bool, now: SystemTime) -> Self {
        let (config, status) = match RelayConfig::load(&config_path) {
            Ok(config) => (config, MeshStatus::Ready),
            Err(tilo_relay_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && config_path.parent().is_some_and(|dir| !dir.is_dir()) =>
            {
                tracing::warn!(
                    target: "tilo::settings",
                    config = %config_path.display(),
                    "this device's sync settings are not visible from here"
                );
                let config = RelayConfig {
                    work: WorkSetting::none(),
                    services: tilo_relay_protocol::ServiceCapabilities::default(),
                    ..RelayConfig::default()
                };
                (config, MeshStatus::ConfigUnreadable)
            }
            Err(tilo_relay_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                let config = RelayConfig {
                    work: WorkSetting::none(),
                    services: tilo_relay_protocol::ServiceCapabilities::default(),
                    ..RelayConfig::default()
                };
                (config, MeshStatus::NotConfigured)
            }
            Err(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    config = %config_path.display(),
                    "this device's sync settings could not be read"
                );
                (RelayConfig::default(), MeshStatus::ConfigUnreadable)
            }
        };
        let runtime_status = RelayRuntimeStatus::read(&config, now);
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
            status,
            runtime_status,
            home_peers,
            home_peer_last_seen: if devices {
                load_home_peer_last_seen()
            } else {
                HashMap::new()
            },
            roaming_provenance_path,
            roaming_provenance,
            session_devices: devices
                .then(|| {
                    ui::collaboration::SessionResource::open(SESSION_DEVICES)
                        .inspect_err(|error| {
                            tracing::debug!(
                                target: "tilo::settings",
                                %error,
                                "the session's devices are not read here"
                            );
                        })
                        .ok()
                })
                .flatten(),
        }
    }
}

impl DeviceMeshModel {
    #[cfg(test)]
    fn from_path(config_path: PathBuf, devices: bool) -> Self {
        Self::from_read(
            devices,
            MeshRead::of(config_path, devices, ui::time::wall()),
        )
    }

    fn from_read(devices: bool, read: MeshRead) -> Self {
        let (roaming_provenance, roaming_provenance_unreadable, roaming_provenance_stamp) =
            read.roaming_provenance;
        Self {
            config_path: read.config_path,
            config: read.config,
            status: read.status,
            devices,
            changed: false,
            runtime_status: read.runtime_status,
            home_peers: read.home_peers,
            home_peer_health: HashMap::new(),
            home_peer_last_seen: read.home_peer_last_seen,
            personal_devices: Vec::new(),
            personal_devices_unavailable: false,
            session_devices: read.session_devices.inspect(|resource| {
                resource.attach(ui::WakeHandle::new());
            }),
            nearby: std::collections::BTreeSet::new(),
            device_rows: DisclosureGroup::new(),
            removing: None,
            roaming_provenance_path: read.roaming_provenance_path,
            roaming_provenance_stamp,
            roaming_provenance,
            roaming_provenance_unreadable,
            management_child: None,
            management_device: None,
            management_call: None,
            devices_call: None,
            home_relay: home_relay::HomeRelay::default(),
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
            pages: std::rc::Weak::new(),
            health_round: 0,
            removals_pending: false,
        }
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        let mut pane = Self::from_path(path.to_path_buf(), false);
        pane.roaming_provenance_path =
            Some(path.with_file_name(settings_store::roaming_provenance::FILE_NAME));
        pane.refresh_roaming_provenance();
        pane.changed = false;
        pane.next_status_refresh = ui::time::now() + Duration::from_secs(24 * 60 * 60);
        pane
    }

    fn apply(&mut self, effect: &DeviceMeshEffect) -> Result<(), String> {
        self.follow_removal(effect);
        let configured = match effect {
            DeviceMeshEffect::SetWork { work } => {
                let from =
                    work_from_label(work).ok_or_else(|| "unsupported work setting".to_string())?;
                self.apply_work(work_from(from));
                true
            }
            DeviceMeshEffect::SetDeviceService { service, on } => {
                match service.as_str() {
                    "full_content_replication" => {
                        self.config.services.full_content_replication = *on;
                    }
                    "inbound_relay" => self.config.services.inbound_relay = *on,
                    "remote_control_relay" => self.config.services.remote_control_relay = *on,
                    "app_serving" => self.config.services.app_serving = *on,
                    "compute" => self.config.services.compute = *on,
                    _ => return Err("unsupported device service".into()),
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
            DeviceMeshEffect::SetHomeRelayHosting { workspace, on } => {
                self.choose_home_relay(workspace, *on, None)?;
                false
            }
            DeviceMeshEffect::ReviewRemoval { .. } | DeviceMeshEffect::CancelRemoval => false,
        };
        if configured {
            self.persist();
        }
        self.changed = true;
        Ok(())
    }

    fn follow_removal(&mut self, effect: &DeviceMeshEffect) {
        match effect {
            DeviceMeshEffect::ReviewRemoval { device } => {
                self.removing = Some(device.clone());
                self.device_rows.open(device);
            }
            DeviceMeshEffect::CancelRemoval | DeviceMeshEffect::RemoveHomeServer { .. } => {
                self.removing = None;
            }
            _ => {}
        }
    }

    fn refresh_personal_devices(&mut self) {
        if self.devices_call.is_none() {
            self.devices_call = Some(crate::device_fleet::list(&self.remote));
        }
    }

    fn poll_host_calls(&mut self) {
        if self.devices_call.is_some()
            || self.management_call.is_some()
            || self.home_relay.waiting()
        {
            self.remote.poll();
        }
        self.poll_home_relay();
        if let Some(answer) = self
            .devices_call
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.devices_call = None;
            match answer.and_then(|output| crate::device_fleet::devices(output.value)) {
                Ok(devices) => {
                    self.personal_devices = devices;
                    self.personal_devices_unavailable = false;
                }
                Err(error) => {
                    tracing::warn!(
                        target: "tilo::settings",
                        %error,
                        "the host's device list could not be read"
                    );
                    self.personal_devices_unavailable = true;
                }
            }
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
    }

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
        self.management_call = Some((call, Some(peer.device_node_id.clone())));
        self.home_peer_health
            .insert(peer.device_node_id.clone(), HomePeerHealth::Connecting);
        self.changed = true;
    }

    fn take_management_answer(
        &mut self,
        device: Option<String>,
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
                if let Some(device) = device {
                    self.home_peer_health.insert(
                        device.clone(),
                        if removed {
                            HomePeerHealth::Removed
                        } else {
                            HomePeerHealth::Online { files_ready: ready }
                        },
                    );
                    self.home_peer_last_seen
                        .insert(device, ui::time::unix_now_ms());
                    save_home_peer_last_seen(&self.home_peer_last_seen);
                }
                self.home_peers = registered_home_peers();
            }
            Ok(None) => {}
            Err(detail) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %detail,
                    "the home server did not answer"
                );
                if let Some(device) = device {
                    self.home_peer_health
                        .insert(device, HomePeerHealth::Unreachable);
                }
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
        let (projection, unreadable, stamp) =
            read_roaming_provenance(self.roaming_provenance_path.as_deref());
        self.roaming_provenance = projection;
        self.roaming_provenance_unreadable = unreadable;
        self.roaming_provenance_stamp = stamp;
        self.changed = true;
    }

    fn apply_work(&mut self, work: WorkSetting) -> bool {
        if work == self.config.work {
            return false;
        }
        let takes = takes_work(&work);
        if takes != takes_work(&self.config.work) {
            self.changed = true;
        }
        self.config.work = work;
        // A device that takes no work runs no service for others.
        self.config.services.workspace_replication = takes;
        self.config.services.inbound_relay = takes;
        self.config.services.full_content_replication &= takes;
        self.next_status_refresh = ui::time::now();
        true
    }
}

pub(super) struct Page<'a> {
    model: &'a DeviceMeshModel,
    pub(super) cx: &'a ViewCx<Settings>,
    actions: Actions<Settings>,
    dispatcher: ui::framework::Dispatcher<Settings>,
}

impl std::ops::Deref for Page<'_> {
    type Target = DeviceMeshModel;

    fn deref(&self) -> &DeviceMeshModel {
        self.model
    }
}

impl Page<'_> {
    pub(super) fn bind<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        control: C,
        effect: impl tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        self.actions.bind_effect(control, effect)
    }

    pub(super) fn bind_invocation<C: ui::framework::ActionControl<()>>(
        &self,
        control: C,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        self.actions.bind_effect_invocation(control, &effect.into())
    }

    pub(super) fn bind_on_row<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        row: &ui::tilo_actions::ResourceSpec,
        control: C,
        effect: impl tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        crate::on_row::bind(&self.dispatcher, row, control, effect)
    }

    pub(super) fn bind_row<C: ui::framework::ActionControl<()>>(
        &self,
        control: C,
        row: &ui::tilo_actions::ResourceSpec,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        crate::on_row::bind_invocation(&self.dispatcher, row, control, effect)
    }

    pub(super) fn switch_on_row(
        &self,
        label: String,
        description: String,
        id: &str,
        on: bool,
        row: &ui::tilo_actions::ResourceSpec,
        set: impl tilo_resource::Binds<SettingsEffect, bool>,
    ) -> provider_settings_ui::widgets::Item {
        let toggle = self.bind_on_row(
            row,
            ui::widget::Toggle::from_state(&LocalState::new(on)).with_id(id),
            set,
        );
        self.cx
            .setting(label)
            .described(description)
            .control(Box::new(toggle))
    }

    pub(super) fn switch(
        &self,
        label: String,
        description: String,
        id: String,
        on: bool,
        set: impl tilo_resource::Binds<SettingsEffect, bool>,
    ) -> provider_settings_ui::widgets::Item {
        self.cx
            .setting(label)
            .described(description)
            .id(id)
            .toggle_effect(&LocalState::new(on), &self.actions, set)
    }

    fn page(&self) -> Element {
        let activity = t("mesh-activity");
        let sync_auto = t("mesh-sync-auto");
        let mut continuation_rows = vec![
            value_row(&activity, &sync_auto),
            note(&t("mesh-activity-caption")),
        ];
        if let Some(trouble) = self.status.settings_trouble_shown() {
            continuation_rows.push(row(error_caption(&trouble)));
        }

        let target_name = self
            .personal_devices
            .iter()
            .find(|device| !device.current && !device.revoked)
            .map_or_else(|| t("mesh-no-target"), |device| device.label.clone());

        let mut roaming_rows = if self.roaming_provenance_unreadable {
            vec![note(&t("mesh-roaming-unreadable"))]
        } else {
            self.roaming_provenance.as_ref().map_or_else(
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
            )
        };
        roaming_rows.push(note(&t("mesh-roaming-caption")));

        let mut groups = vec![self.devices_section().into_element()];
        groups.extend(self.home_relay_section().map(SettingsSection::into_element));
        groups.push(section(&t("mesh-continue-title"), rows(continuation_rows)).into_element());
        groups.extend(
            self.continuity_group(move |cx, continuity| {
                privacy_section(cx, continuity, &target_name)
            }),
        );
        groups.push(section(&t("mesh-roaming-title"), rows(roaming_rows)).into_element());
        groups.extend(self.continuity_group(|_, continuity| activity_section(continuity)));
        groups.extend(self.agents_group());
        page_of(groups)
    }

    fn continuity_group(
        &self,
        build: impl Fn(&ViewCx<ContinuityPrivacy>, &ContinuityPrivacy) -> SettingsSection + 'static,
    ) -> Option<Element> {
        let continuity = self.cx.settings::<ContinuityPrivacy>();
        let build = Rc::new(build);
        self.cx.record(&continuity, move |cx| {
            let build = Rc::clone(&build);
            cx.switch(
                &cx.value(ContinuityPrivacy::clone),
                move |cx, continuity| build(cx, continuity).into_element(),
            )
        })
    }

    fn agents_group(&self) -> Option<Element> {
        let door = self.cx.settings::<crate::prefs::AgentDoorDevice>();
        self.cx.record(&door, |cx| {
            section(
                &t("mesh-agents-title"),
                rows(vec![
                    cx.setting(t("mesh-agents-accept"))
                        .described(t("mesh-agents-accept-desc"))
                        .id(AGENTS_ACCEPT_KEY)
                        .toggle_verb(
                            |door: &crate::prefs::AgentDoorDevice| door.accept_own_devices,
                            crate::prefs::AgentDoorDevice::SET_ACCEPT_OWN_DEVICES,
                        ),
                ]),
            )
            .into_element()
        })
    }

    fn service_row(
        &self,
        label: &str,
        description: &str,
        service: &'static str,
        id: &str,
        on: bool,
    ) -> provider_settings_ui::widgets::Item {
        self.switch(
            t(label),
            t(description),
            id.to_owned(),
            on,
            effect(DeviceMeshEffect::SET_DEVICE_SERVICE)
                .map(move |on: bool| (service.to_owned(), on)),
        )
    }
}

fn privacy_section(
    cx: &ViewCx<ContinuityPrivacy>,
    continuity: &ContinuityPrivacy,
    target: &str,
) -> SettingsSection {
    let mut privacy_rows = vec![
        cx.setting(t("mesh-allow-continuity"))
            .described(t("mesh-allow-continuity-desc"))
            .id(CONTINUITY_ENABLED_KEY)
            .toggle_verb(
                |continuity: &ContinuityPrivacy| continuity.enabled,
                ContinuityPrivacy::ALLOW_CONTINUITY,
            ),
    ];
    for capability in Capability::ALL {
        let key = continuity_key(capability);
        let desc = continuity::description(capability);
        let status = if continuity.effective(capability) {
            t_args(
                "mesh-facet-active",
                &[("desc", desc.as_str().into()), ("target", target.into())],
            )
        } else if continuity.allowed(capability) {
            t_args("mesh-facet-prepared", &[("desc", desc.into())])
        } else {
            t_args("mesh-facet-off", &[("desc", desc.into())])
        };
        let facet = capability.key().to_owned();
        privacy_rows.push(
            cx.setting(continuity::label(capability))
                .described(status)
                .id(key)
                .toggle_verb(
                    move |continuity: &ContinuityPrivacy| continuity.allowed(capability),
                    ContinuityPrivacy::ALLOW_CAPABILITY.map(move |on: bool| (facet.clone(), on)),
                ),
        );
    }
    privacy_rows.push(note(&t("mesh-privacy-stay")));
    section(&t("mesh-privacy-title"), rows(privacy_rows))
}

fn activity_section(continuity: &ContinuityPrivacy) -> SettingsSection {
    let audit_rows = if continuity.audit.is_empty() {
        vec![note(&t("mesh-audit-empty"))]
    } else {
        continuity
            .audit
            .iter()
            .rev()
            .take(6)
            .map(|entry| {
                let change = t("mesh-audit-change");
                value_row(&change, &crate::continuity::summary(entry))
            })
            .collect()
    };
    section(&t("mesh-activity-title"), rows(audit_rows))
}

impl DeviceMeshModel {
    fn persist(&mut self) {
        if self.config_path == std::path::Path::new(DEFAULT_CONFIG) {
            if self.apply_child.is_some() {
                self.apply_queued = true;
                self.status = MeshStatus::Ready;
            } else {
                self.start_apply();
            }
            return;
        }
        self.status = match self.config.save(&self.config_path) {
            Ok(()) => MeshStatus::Ready,
            Err(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    config = %self.config_path.display(),
                    "this device's sync settings could not be saved"
                );
                MeshStatus::NotSaved
            }
        };
    }

    fn spawn_apply(&self) -> Result<bounded_command::AsyncCommand, bounded_command::Error> {
        let request = serde_json::json!({
            "identity_root": identity_root(),
            "work": self.config.work,
            "workspace_ids": Vec::<u64>::new(),
            "full_content_replication": self.config.services.full_content_replication,
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
    }

    fn start_apply(&mut self) {
        match self.spawn_apply() {
            Ok(child) => {
                self.apply_child = Some(child);
                self.status = MeshStatus::Ready;
            }
            Err(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "this device's sync setup could not be started"
                );
                self.status = MeshStatus::NotSaved;
            }
        }
    }

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
            self.status = MeshStatus::Looking { target };
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
            arguments.insert("workspace_ids".into(), Vec::<u64>::new().into());
        }
        arguments.insert("confirm".into(), true.into());
        self.pairing_call = Some(self.remote.invoke(
            device_pairing::target(device_pairing::APPROVE),
            InvokeArgument::Json(arguments),
            Some(Confirmation::Confirmed),
        ));
        self.pairing_discovery = false;
        self.status = MeshStatus::Approving {
            label: candidate.label,
            code: candidate.verification_code,
        };
        self.changed = true;
    }

    fn poll_pairing(&mut self) {
        use ui::tilo_actions::InvocationUpdate;
        let Some(call) = self.pairing_call.as_ref() else {
            return;
        };
        self.remote.poll();
        let answer = call.get();
        if !answer.is_finished() {
            return;
        }
        self.pairing_call = None;
        let discovery = std::mem::take(&mut self.pairing_discovery);
        self.changed = true;
        let output = match answer {
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => output,
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                tracing::warn!(
                    target: "tilo::settings",
                    message = %error.message,
                    "pairing did not finish"
                );
                self.status = MeshStatus::PairingStopped { cancelled: false };
                return;
            }
            ui::Arrival::Ready(_) => {
                self.status = MeshStatus::PairingStopped { cancelled: true };
                return;
            }
            ui::Arrival::Failed(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "pairing did not finish"
                );
                self.status = MeshStatus::PairingStopped { cancelled: false };
                return;
            }
            ui::Arrival::Pending { .. } => return,
        };
        if discovery {
            match output
                .value
                .map(serde_json::from_value::<PairCandidate>)
                .transpose()
            {
                Ok(Some(candidate)) => {
                    self.status = MeshStatus::Found {
                        label: candidate.label.clone(),
                        code: candidate.verification_code.clone(),
                    };
                    self.pairing_candidate = Some(candidate);
                }
                Ok(None) => {
                    tracing::warn!(
                        target: "tilo::settings",
                        "nearby discovery found no device to pair"
                    );
                    self.status = MeshStatus::DiscoveryUnusable;
                }
                Err(error) => {
                    tracing::warn!(
                        target: "tilo::settings",
                        %error,
                        "the device nearby discovery found is not readable"
                    );
                    self.status = MeshStatus::DiscoveryUnusable;
                }
            }
            return;
        }
        self.refresh_runtime_status();
        self.status = match self.pairing_target {
            PairingTarget::HomePeer => MeshStatus::HomeServerPaired,
            PairingTarget::PersonalDevice => {
                self.refresh_personal_devices();
                MeshStatus::DeviceAdded
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
                    .insert(peer.device_node_id.clone(), HomePeerHealth::Connecting);
            }
            Err(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    action,
                    "the home server command did not start"
                );
                self.home_peer_health
                    .insert(peer.device_node_id.clone(), HomePeerHealth::Unreachable);
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
            use ui::tilo_actions::remote::{InvokeArgument, identity_admin};
            let call = self.remote.invoke(
                identity_admin::target(identity_admin::HOME_PEER_RETRY),
                InvokeArgument::None,
                None,
            );
            self.management_call = Some((call, None));
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
                        self.status = MeshStatus::Ready;
                    }
                } else {
                    self.apply_queued = false;
                    tracing::warn!(
                        target: "tilo::settings",
                        detail = %command_error(
                            &output.stderr,
                            "administrator approval was not granted"
                        ),
                        "this device's sync settings could not be applied"
                    );
                    self.status = MeshStatus::NotSaved;
                }
                self.changed = true;
            }
            Some(Err(error)) => {
                self.apply_child = None;
                self.apply_queued = false;
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "this device's sync settings could not be applied"
                );
                self.status = MeshStatus::NotSaved;
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
                let device = self.management_device.take();
                let answer = if output.status.success() {
                    Ok(serde_json::from_slice::<serde_json::Value>(&output.stdout).ok())
                } else {
                    Err(command_error(&output.stderr, "not reachable"))
                };
                self.take_management_answer(device, answer);
            }
            Some(Err(error)) => {
                self.management_child = None;
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "the home server command did not finish"
                );
                if let Some(device) = self.management_device.take() {
                    self.home_peer_health
                        .insert(device, HomePeerHealth::Unreachable);
                }
                self.changed = true;
            }
            None => {}
        }
    }
}

impl DeviceMeshModel {
    fn shown(&self) -> bool {
        self.pages.strong_count() > 0
    }

    fn show(&mut self) -> Rc<()> {
        if let Some(held) = self.pages.upgrade() {
            return held;
        }
        let held = Rc::new(());
        self.pages = Rc::downgrade(&held);
        held
    }

    /// Take in what the session last said of which devices are nearby.
    fn take_nearby(&mut self) {
        let Some(Ok(reading)) = self
            .session_devices
            .as_ref()
            .and_then(ui::collaboration::SessionResource::latest)
        else {
            return;
        };
        if let Some(nearby) = nearby_in(&reading).filter(|nearby| *nearby != self.nearby) {
            self.nearby = nearby;
            self.changed = true;
        }
    }

    fn poll(&mut self) -> bool {
        self.poll_background_tasks();
        self.take_nearby();
        if let Some(follow) = self.following.upgrade() {
            self.take_followed(&follow);
        } else if ui::time::now() >= self.next_status_refresh {
            self.refresh_runtime_status();
            self.refresh_one_home_peer();
        }
        std::mem::take(&mut self.changed)
    }

    fn busy(&self) -> bool {
        self.apply_child.is_some() || self.pairing_call.is_some() || self.management_child.is_some()
    }

    fn next_wake(&self) -> Option<Instant> {
        if !takes_work(&self.config.work) {
            return None;
        }
        match self.following.upgrade() {
            Some(follow) => (!self.home_peers.is_empty()
                || self.removals_pending
                || !follow.files.is_complete())
            .then_some(self.next_status_refresh),
            None => Some(self.next_status_refresh),
        }
    }

    fn follow(&mut self) -> Option<Rc<MeshFollow>> {
        if !self.devices || !takes_work(&self.config.work) {
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
        self.health_round = self.home_peers.len().max(1);
        self.next_status_refresh = ui::time::now();
        Some(follow)
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        Page {
            model: self,
            cx,
            actions: cx.actions(),
            dispatcher: cx.dispatcher(),
        }
        .page()
    }
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    let showing = model.borrow_mut().show();
    let following = model.borrow_mut().follow();
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| {
        let _held = (&showing, &following);
        model.borrow().page(cx)
    }))
}

impl DeviceMeshModel {
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
    use ui::prelude::TestApp;
    use ui::widget::{DisclosureRow, Widget};

    use crate::resource::SettingsShell;

    fn window_on(model: DeviceMeshModel) -> TestApp<SettingsShell> {
        crate::area::replace_in_next_window(move |context| DeviceMeshArea::over(context, model));
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::DeviceMesh);
        app
    }

    fn change_model<T: 'static>(
        app: &mut TestApp<SettingsShell>,
        change: impl FnOnce(&mut DeviceMeshModel) -> T + 'static,
    ) -> T {
        let changed = app
            .with_widget::<crate::window::Inspected, _>(move |settings| {
                change(&mut area(settings).unwrap().model().borrow_mut())
            })
            .expect("the Settings window is mounted");
        app.settle();
        changed
    }

    fn millis_since_epoch(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            })
    }

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
    fn invisible_relay_state_is_unreadable_not_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join("etc-tilo").join("relay.toml");
        let pane = DeviceMeshModel::with_path(&hidden);
        assert_eq!(pane.status, MeshStatus::ConfigUnreadable);
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

    #[test]
    fn the_relay_status_follows_its_heartbeat_and_its_lapse() {
        let held = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = {
            let held = Arc::clone(&held);
            Arc::new(move || {
                SystemTime::UNIX_EPOCH
                    + Duration::from_millis(held.load(std::sync::atomic::Ordering::Acquire))
            }) as StatusClock
        };
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("tilo-relay");
        std::fs::create_dir_all(&state_root).unwrap();
        let config = RelayConfig {
            full_replica_status: state_root.join("full-replica.json"),
            state_root,
            work: WorkSetting::own_person(),
            ..RelayConfig::default()
        };
        let follow = MeshFollow::start_at(&config, &[], clock).expect("the status thread starts");
        tilo_test_env::wait_until("the first read", || {
            follow.status() == Some(RelayRuntimeStatus::Offline)
        });
        let metrics = tilo_relay_protocol::metrics::MetricsSnapshot::default();
        tilo_relay_protocol::metrics::save_metrics(&config.state_root, &metrics).unwrap();
        let written = std::fs::metadata(
            config
                .state_root
                .join(tilo_relay_protocol::metrics::METRICS_FILE),
        )
        .and_then(|metadata| metadata.modified())
        .unwrap();
        held.store(
            millis_since_epoch(written),
            std::sync::atomic::Ordering::Release,
        );
        tilo_test_env::wait_until("the heartbeat to show the relay online", || {
            follow.status() == Some(RelayRuntimeStatus::Online)
        });
        held.store(
            millis_since_epoch(written + STATUS_FRESHNESS + Duration::from_secs(1)),
            std::sync::atomic::Ordering::Release,
        );
        tilo_test_env::wait_until("the lapsed heartbeat to show it offline", || {
            follow.status() == Some(RelayRuntimeStatus::Offline)
        });
    }

    #[test]
    fn a_followed_pane_reads_on_a_change_and_sets_no_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", dir.path());
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        pane.config.work = WorkSetting::own_person();
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

    #[test]
    fn continuity_controls_are_typed_audited_and_refuse_future_provenance() {
        {
            let dir = tempfile::tempdir().unwrap();
            let pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
            let mut app = window_on(pane);
            let ids = test_ids(app.harness().root_widget());

            assert!(ids.iter().any(|id| id == CONTINUITY_ENABLED_KEY));
            for capability in Capability::ALL {
                let key = continuity_key(capability);
                assert!(ids.iter().any(|id| id == &key), "missing {key}");
            }
        }

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

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir
                .path()
                .join(settings_store::roaming_provenance::FILE_NAME);
            std::fs::write(&path, r#"{"schema_version":99}"#).unwrap();
            let (projection, unreadable, _) = read_roaming_provenance(Some(&path));
            assert!(projection.is_none());
            assert!(unreadable, "the schema is not one this device knows");
        }

        {
            use crate::testing::{CONTINUITY, invoke_section};
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
                serde_json::json!({ "capability": "clipboard", "on": false }),
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
            assert!(!saved.effective(Capability::Clipboard) && saved.effective(Capability::Media));
            assert_eq!(saved.audit.len(), 2);
            let encoded = serde_json::to_string(&saved.audit).unwrap();
            for forbidden in ["payload", "token", "address", "file_name"] {
                assert!(!encoded.contains(forbidden));
            }
        }
    }

    #[test]
    fn relay_config_edits_are_real_honest_and_coalesced() {
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            RelayConfig::default().save(&path).unwrap();

            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            pane.apply(&DeviceMeshEffect::SetWork {
                work: WORK_ANYONE.into(),
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetDeviceService {
                service: "full_content_replication".into(),
                on: true,
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetDeviceService {
                service: "remote_control_relay".into(),
                on: false,
            })
            .unwrap();

            let saved = RelayConfig::load(&path).unwrap();
            assert_eq!(saved.work.from, WorkFrom::Anyone);
            assert!(saved.services.full_content_replication);
            assert!(!saved.services.remote_control_relay);
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let mut pane = DeviceMeshModel::with_path(&path);
            assert_eq!(pane.status, MeshStatus::NotConfigured);
            assert!(pane.config.work.takes_nothing());
            assert_eq!(
                pane.config.services,
                tilo_relay_protocol::ServiceCapabilities::default()
            );
            let _ = pane.poll();
            assert!(!path.exists());
            pane.apply(&DeviceMeshEffect::SetWork {
                work: WORK_OWN.into(),
            })
            .unwrap();
            assert!(path.exists());
        }

        {
            assert_eq!(work_from_label(WORK_NOTHING), Some(WorkFrom::Nobody));
            assert_eq!(work_from_label(WORK_OWN), Some(WorkFrom::OwnDevices));
            assert_eq!(work_from_label(WORK_ANYONE), Some(WorkFrom::Anyone));
            assert_eq!(work_from_label("Personal device"), None);
            assert!(work_from(WorkFrom::Nobody).takes_nothing());
            assert!(!work_from(WorkFrom::OwnDevices).takes_nothing());
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
            pane.config_path = PathBuf::from(DEFAULT_CONFIG);
            let mut command = Command::new("sleep");
            command.arg("infinity");
            pane.apply_child = Some(
                bounded_command::spawn_prepared(command, None, 64, tilo_test_env::hang_bound())
                    .unwrap(),
            );

            pane.persist();
            pane.poll_background_tasks();

            assert!(pane.apply_child.is_some(), "the running apply is kept");
            assert!(pane.apply_queued);
            pane.apply_child = None;
        }
    }

    #[test]
    fn the_work_a_device_takes_and_its_services_stay_local() {
        {
            assert!(!takes_work(&WorkSetting::none()));
            assert!(takes_work(&WorkSetting::own_person()));
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let config = RelayConfig {
                work: WorkSetting::none(),
                ..RelayConfig::default()
            };
            config.save(&path).unwrap();
            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            assert!(!pane.poll());

            pane.apply(&DeviceMeshEffect::SetWork {
                work: WORK_OWN.into(),
            })
            .unwrap();

            assert!(pane.poll());
            assert_eq!(pane.config.work.from, WorkFrom::OwnDevices);
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let config = RelayConfig {
                work: WorkSetting::none(),
                services: tilo_relay_protocol::ServiceCapabilities::default(),
                ..RelayConfig::default()
            };
            config.save(&path).unwrap();

            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();
            pane.apply(&DeviceMeshEffect::SetWork {
                work: WORK_ANYONE.into(),
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetDeviceService {
                service: "full_content_replication".into(),
                on: true,
            })
            .unwrap();

            let saved = RelayConfig::load(&path).unwrap();
            assert_eq!(saved.work.from, WorkFrom::Anyone);
            assert!(saved.services.workspace_replication);
            assert!(saved.services.full_content_replication);
            assert!(saved.services.inbound_relay);
        }
    }

    #[test]
    fn a_pairing_that_did_not_finish_says_what_to_do_not_the_hosts_words() {
        const HOST_WORDS: &str = "the code the second screen showed did not match";
        let dir = tempfile::tempdir().unwrap();
        let mut model = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        model.remote = ui::RemoteActions::with_host(
            Arc::new(PairingHost {
                approval_failure: Some(HOST_WORDS.into()),
                ..PairingHost::default()
            }),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );
        model.apply(&DeviceMeshEffect::PairHomeServer).unwrap();
        model.poll_background_tasks();
        model.apply(&DeviceMeshEffect::PairHomeServer).unwrap();
        model.poll_background_tasks();
        assert_eq!(
            model.status,
            MeshStatus::PairingStopped { cancelled: false }
        );

        let mut app = window_on(model);
        app.settle();
        let shown =
            provider_settings_ui::widgets::announced_text(app.harness().root_widget()).join("\n");
        assert!(
            shown.contains(&t("mesh-status-pairing-failed")),
            "the pane says what to do: {shown}"
        );
        assert!(
            !shown.contains(HOST_WORDS),
            "the host's words stay in the log: {shown}"
        );
    }

    #[test]
    fn the_panes_troubles_read_in_the_persons_language() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        model.status = MeshStatus::NotSaved;
        model.personal_devices_unavailable = true;
        model.roaming_provenance_unreadable = true;
        let mut app = window_on(model);
        app.settle();
        let shown =
            provider_settings_ui::widgets::announced_text(app.harness().root_widget()).join("\n");
        for message in [
            t("mesh-status-not-saved"),
            t("mesh-status-device-list-unavailable"),
            t("mesh-roaming-unreadable"),
        ] {
            assert!(shown.contains(&message), "{message} is shown: {shown}");
        }
        assert!(MeshStatus::Ready.settings_trouble_shown().is_none());
        assert!(MeshStatus::NotConfigured.pairing_shown().is_none());
        assert_eq!(
            MeshStatus::ConfigUnreadable.settings_trouble_shown(),
            Some(t("mesh-status-config-unreadable"))
        );
        assert_eq!(
            MeshStatus::PairingStopped { cancelled: true }.pairing_shown(),
            Some(t("mesh-status-pairing-cancelled"))
        );
    }

    #[test]
    fn device_health_and_workspace_status_reflect_fresh_facts() {
        {
            assert_eq!(
                peer_health_badge(HomePeerHealth::Online { files_ready: true }),
                "Online"
            );
            assert_eq!(
                peer_health_badge(HomePeerHealth::Online { files_ready: false }),
                "Syncing"
            );
            assert_eq!(peer_health_badge(HomePeerHealth::Unreachable), "Offline");
            assert_eq!(peer_health_badge(HomePeerHealth::Connecting), "Checking");
            assert_eq!(
                peer_health_detail(HomePeerHealth::Unreachable),
                "Offline. Check that it is on and connected"
            );
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let config = RelayConfig {
                state_root: dir.path().to_path_buf(),
                work: WorkSetting::own_person(),
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

        {
            let dir = tempfile::tempdir().unwrap();
            let proof = dir.path().join("full-replica.json");
            let config = RelayConfig {
                state_root: dir.path().to_path_buf(),
                full_replica_status: proof.clone(),
                work: WorkSetting::own_person(),
                services: tilo_relay_protocol::ServiceCapabilities::personal_server(),
                ..RelayConfig::default()
            };
            tilo_relay_protocol::metrics::save_metrics(
                &config.state_root,
                &tilo_relay_protocol::metrics::MetricsSnapshot::default(),
            )
            .unwrap();
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

    fn row_holding(widget: &dyn Widget, id: &str) -> Option<String> {
        if let Some(found) = widget
            .children()
            .iter()
            .find_map(|child| row_holding(*child, id))
        {
            return Some(found);
        }
        let is_row = widget
            .as_any()
            .is_some_and(<dyn std::any::Any>::is::<DisclosureRow>);
        (is_row && ui::widget::find_test_id(widget, id).is_some())
            .then(|| widget.accessibility().and_then(|node| node.name))
            .flatten()
            .map(|name| name.split(", ").next().unwrap_or_default().to_owned())
    }

    fn devices_and_a_home_server(dir: &std::path::Path) -> DeviceMeshModel {
        let mut pane = DeviceMeshModel::with_path(&dir.join("relay.toml"));
        let device = |key: &str, label: &str, current, can_host| crate::device_fleet::DeviceView {
            label: label.into(),
            key: hex::decode(key).unwrap().try_into().unwrap(),
            fingerprint: format!("fp {label}"),
            current,
            revoked: false,
            can_host,
            kind: ui::tilo_actions::remote::identity_admin::KeyKind::Device,
            ..crate::device_fleet::DeviceView::default()
        };
        pane.personal_devices = vec![
            device(THIS_DEVICE, "Ada's Laptop", true, false),
            device(PHONE, "Ada's Phone", false, false),
            device(NEARBY_DEVICE, "Kitchen NAS", false, true),
        ];
        pane.home_peers = vec![
            serde_json::from_value(serde_json::json!({
                "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                "device_node_id": NEARBY_DEVICE,
                "relay_service_node_id": NEARBY_SERVICE,
                "label": "Kitchen NAS",
                "user_public_key": "33".repeat(32),
                "relay_workspace_ids": [],
            }))
            .unwrap(),
        ];
        pane
    }

    /// A device the session hears on this device's network says so on its row; the others keep
    /// what they are.
    #[test]
    fn a_device_the_session_hears_nearby_says_so() {
        let _settled = ui::motion::testing::settled();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = devices_and_a_home_server(dir.path());
        pane.nearby = nearby_in(
            &serde_json::json!({"devices": [
                {"id": PHONE.to_ascii_uppercase(), "nearby": true},
                {"id": THIS_DEVICE, "nearby": false},
            ]})
            .to_string(),
        )
        .unwrap();
        let mut app = window_on(pane);
        let shown = provider_settings_ui::widgets::announced_text(app.harness().root_widget());
        assert!(
            shown
                .iter()
                .any(|line| line.starts_with("Ada's Phone, Nearby")),
            "{shown:#?}"
        );
        assert!(
            !shown
                .iter()
                .any(|line| line.starts_with("Ada's Laptop, Nearby")),
            "{shown:#?}"
        );
    }

    #[test]
    fn each_device_is_one_row_whose_settings_open_under_it() {
        let _settled = ui::motion::testing::settled();
        let dir = tempfile::tempdir().unwrap();
        let mut app = window_on(devices_and_a_home_server(dir.path()));
        let text = |app: &mut TestApp<SettingsShell>| {
            provider_settings_ui::widgets::announced_text(app.harness().root_widget())
        };
        let shown = text(&mut app);
        for name in ["Ada's Laptop", "Ada's Phone", "Kitchen NAS"] {
            let rows = shown
                .iter()
                .filter(|line| line.starts_with(&format!("{name}, ")))
                .count();
            assert_eq!(rows, 1, "{name} is one row of the list: {shown:#?}");
        }
        for gone in [
            "Decommission",
            "Apply name",
            "Current target",
            "Synced workspaces",
            "Paired home servers",
        ] {
            assert!(
                !shown.iter().any(|line| line.contains(gone)),
                "{gone}: {shown:#?}"
            );
        }

        {
            let composed = app.harness().root_widget();
            let holder = |id: &str| row_holding(composed, id);
            assert_eq!(holder("device_mesh.work").as_deref(), Some("Ada's Laptop"));

            let rename = peer_key("name", NEARBY_DEVICE);
            assert_eq!(holder(&rename).as_deref(), Some("Kitchen NAS"));
            let field = ui::widget::find_test_id(composed, &rename).unwrap();
            assert_eq!(
                field.accessibility().and_then(|node| node.name).as_deref(),
                Some(t("mesh-name").as_str()),
                "the name is its own field"
            );
            let ids = test_ids(composed);
            assert!(
                !ids.iter()
                    .any(|id| *id == peer_key("rename", NEARBY_DEVICE)),
                "no separate button applies the name: {ids:?}"
            );
        }

        let manage = personal_device_key("manage-access", PHONE);
        let ids = test_ids(app.harness().root_widget());
        assert!(ids.contains(&manage), "{ids:?}");
        assert!(
            !ids.iter().any(|id| *id == peer_key("review-remove", PHONE)),
            "{ids:?}"
        );
        assert_eq!(
            change_model(&mut app, |pane| pane.personal_devices.len()),
            3
        );
    }

    #[test]
    fn device_removal_asks_first_and_pairing_is_single_flight() {
        {
            let effect = DeviceMeshEffect::RemoveHomeServer {
                device: "11".repeat(32),
            };
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

    #[derive(Default)]
    pub(super) struct PairingHost {
        asked: Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
        approval_failure: Option<String>,
    }

    impl PairingHost {
        pub(super) fn asked(&self) -> Vec<ui::tilo_actions::remote::InvokeRequest> {
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
            use ui::tilo_actions::remote::{device_pairing, home_relay_hosting, identity_admin};
            let invocation = request.invocation;
            let value = if request.target.provider == home_relay_hosting::PROVIDER {
                let studio_on = self
                    .asked
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|asked| asked.target.address.action == home_relay_hosting::SET);
                serde_json::json!({
                    "relay": "Home server",
                    "workspaces": [
                        { "workspace": super::home_relay_tests::PERSONAL_WORKSPACE, "name": "Personal", "on_relay": true },
                        { "workspace": super::home_relay_tests::STUDIO_WORKSPACE, "name": "Studio", "on_relay": studio_on },
                    ],
                })
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
            let failure = self
                .approval_failure
                .clone()
                .filter(|_| request.target.address.action != device_pairing::DISCOVER);
            self.asked.lock().unwrap().push(request);
            let status = match failure {
                Some(message) => ui::tilo_actions::remote::ReceiptStatus::Failed {
                    error: ui::tilo_actions::ActionFailure {
                        code: "pairing-failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
                None => ui::tilo_actions::remote::ReceiptStatus::Completed {
                    output: ui::tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            };
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::InvocationReceipt { invocation, status },
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
        ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionHostError>>
        {
            ui::tilo_actions::remote::Pending::ready(Ok(()))
        }
    }

    #[test]
    fn the_device_list_is_the_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        pane.remote = ui::RemoteActions::with_host(
            Arc::new(PairingHost::default()),
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
    }

    #[test]
    fn home_server_removal_runs_on_the_host() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        let dir = tempfile::tempdir().unwrap();
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
            pane.home_peer_health.get(NEARBY_DEVICE),
            Some(&HomePeerHealth::Removed)
        );
        assert!(pane.management_call.is_none(), "the removal has answered");
    }

    #[test]
    fn a_home_server_that_does_not_answer_is_offline_not_the_hosts_words() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        pane.take_management_answer(
            Some(NEARBY_DEVICE.into()),
            Err("the home server was not reachable".into()),
        );
        assert_eq!(
            pane.home_peer_health.get(NEARBY_DEVICE),
            Some(&HomePeerHealth::Unreachable)
        );
        assert_eq!(
            peer_health_detail(HomePeerHealth::Unreachable),
            "Offline. Check that it is on and connected"
        );
    }

    #[test]
    fn pairing_invokes_the_host_actions_and_spawns_nothing() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, device_pairing};
        let dir = tempfile::tempdir().unwrap();
        let mut pane = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
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
            pane.status,
            MeshStatus::Found {
                label: "Kitchen NAS".into(),
                code: "123-456".into(),
            }
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
                    "workspace_ids": [],
                    "confirm": true,
                })
                .as_object()
                .unwrap()
                .clone()
            )
        );
        assert_eq!(asked[1].confirmation, Some(Confirmation::Confirmed));
        assert_eq!(pane.status, MeshStatus::HomeServerPaired);
        assert!(
            pane.pairing_candidate.is_none(),
            "the paired device is done"
        );
        assert!(
            pane.apply_child.is_none() && pane.management_child.is_none(),
            "pairing started no process in the app"
        );
    }

    fn until_finished(finished: impl Fn() -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !finished() {
            assert!(std::time::Instant::now() < deadline, "the read ends");
            std::thread::yield_now();
        }
    }

    #[test]
    fn the_area_builds_its_model_from_the_read_it_started() {
        let profile = tempfile::tempdir().unwrap();
        let cx = crate::area::AreaContext::in_profile(profile.path().to_path_buf());
        let area = DeviceMeshArea::new(&cx);
        assert!(area.start_read(), "the first read starts");
        assert!(!area.start_read(), "one read at a time");
        assert!(!area.is_open(), "a running read publishes nothing yet");
        until_finished(|| area.read.is_finished());
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

    #[test]
    fn a_read_builds_the_model_reading_in_place_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        std::fs::write(&path, "iroh_listen = \"0.0.0.0:1234\"\n").unwrap();
        let here = DeviceMeshModel::from_path(path.clone(), false);
        let read = MeshRead::of(path, false, ui::time::wall());
        let there = DeviceMeshModel::from_read(false, read);
        assert_eq!(here.config_path, there.config_path);
        assert_eq!(here.status, there.status);
        assert_eq!(here.config.work, there.config.work);
        assert_eq!(here.runtime_status, there.runtime_status);
    }

    #[test]
    fn a_home_relay_switch_is_named_by_its_workspace_row() {
        use ui::tilo_actions::remote::Caller;

        let dir = tempfile::tempdir().unwrap();
        let mut model = DeviceMeshModel::with_path(&dir.path().join("relay.toml"));
        model.remote = ui::RemoteActions::with_host(
            Arc::new(PairingHost::default()),
            Caller::local_person("owner"),
        );
        let mut app = window_on(model);
        change_model(&mut app, |model| {
            model.refresh_home_relay();
            model.poll_background_tasks();
        });
        let switch = crate::testing::switch_named(&mut app, "Personal");
        assert_eq!(
            switch.toggled(),
            Some(ui::accesskit::Toggled::True),
            "the workspace is hosted on the relay"
        );
    }
}
