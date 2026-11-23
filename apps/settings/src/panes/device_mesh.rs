//! Sync between devices — configure seamless continuation and always-on sync.
//! The pane reads and writes the same `ServerConfig` consumed by `tilo-server`
//! and reads the daemon's once-per-second metrics snapshot for live status.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::time::{Duration, Instant, SystemTime};

use provider_settings_ui::bound::SettingRows as _;
use tilo_server_protocol::{ServerConfig, WorkFrom, WorkSetting, WorkWhen};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource;
#[cfg(test)]
use ui::tilo_resource::Effect;
use ui::widget::{DisclosureGroup, Element};

use crate::area::AreaContext;
use crate::continuity::{self, Capability, ContinuityPrivacy};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    SettingsSection, StatusTone, note, page_of, rows, section, value_row,
};

mod devices;

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

const DEFAULT_CONFIG: &str = "/etc/tilo/server.toml";
const CONTINUITY_ENABLED_KEY: &str = "device_mesh.continuity.enabled";
const AGENTS_ACCEPT_KEY: &str = "device_mesh.agents.accept_own_devices";
const AGENTS_ACT_KEY: &str = "device_mesh.agents.act_without_asking";
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
/// The work this device can take, named by its message: the chooser's values
/// are these, so its labels and the setting a choice writes share one table.
const WORK_FROM: [&str; 3] = ["mesh-work-nothing", "mesh-work-own", "mesh-work-anyone"];

/// The message naming the work a setting takes.
fn work_key(from: &WorkFrom) -> &'static str {
    match from {
        WorkFrom::Nobody => "mesh-work-nothing",
        WorkFrom::OwnDevices => "mesh-work-own",
        WorkFrom::Anyone => "mesh-work-anyone",
        WorkFrom::Workspaces(_) => "mesh-work-some",
    }
}

/// The work a chosen value names, when it is one of the choices.
fn work_from_key(key: &str) -> Option<WorkFrom> {
    [WorkFrom::Nobody, WorkFrom::OwnDevices, WorkFrom::Anyone]
        .into_iter()
        .find(|from| work_key(from) == key)
}

/// The work setting a person picked: every kind of work when it takes any, and none when it takes
/// nobody's, in the states `when` the device took work in before.
fn work_from(from: WorkFrom, when: WorkWhen) -> WorkSetting {
    WorkSetting {
        kinds: if from == WorkFrom::Nobody {
            tilo_server_protocol::WorkKinds::NONE
        } else {
            tilo_server_protocol::WorkKinds::ALL
        },
        from,
        when,
    }
}

/// The shares of the processors or memory a person may keep free from work.
const ROOM: [u8; 3] = [0, 20, 50];

/// How a share of the machine kept free is shown.
fn room_shown(percent: u8) -> String {
    if percent == 0 {
        t("mesh-work-room-none")
    } else {
        t_args(
            "mesh-work-room-percent",
            &[("percent", ui::i18n::ArgValue::Number(f64::from(percent)))],
        )
    }
}

fn room_from_label(label: &str) -> Option<u8> {
    ROOM.into_iter()
        .find(|percent| room_shown(*percent) == label)
}

fn takes_work(work: &WorkSetting) -> bool {
    !work.takes_nothing()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ServerRuntimeStatus {
    #[default]
    Offline,
    Online,
    /// The server's state could not be read from here, which says nothing
    /// about whether the server is running.
    Unreadable,
}

impl ServerRuntimeStatus {
    fn read(config: &ServerConfig, now: SystemTime) -> Self {
        // The state directory's parent always exists on a device; when it is
        // not visible the server cannot be observed, not "offline".
        if config
            .state_root
            .parent()
            .is_some_and(|parent| !parent.is_dir())
        {
            return Self::Unreadable;
        }
        let metrics_path = config
            .state_root
            .join(tilo_server_protocol::metrics::METRICS_FILE);
        let modified =
            match std::fs::metadata(metrics_path).and_then(|metadata| metadata.modified()) {
                Ok(modified) => modified,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Offline,
                Err(_) => return Self::Unreadable,
            };
        if now.duration_since(modified).unwrap_or(Duration::ZERO) > STATUS_FRESHNESS {
            return Self::Offline;
        }
        match tilo_server_protocol::metrics::load_metrics(&config.state_root) {
            Ok(Some(_)) => Self::Online,
            Ok(None) => Self::Offline,
            Err(_) => Self::Unreadable,
        }
    }

    fn label(self) -> String {
        t(match self {
            Self::Offline => "mesh-runtime-offline",
            Self::Online => "mesh-runtime-online",
            Self::Unreadable => "mesh-runtime-unreadable",
        })
    }
}

const HOME_PEER_HEALTH_INTERVAL: Duration = Duration::from_secs(60);

/// The wall clock the server-status thread judges the heartbeat by: the
/// process's, unless a test hands it one that reads fresh however long the
/// thread takes to look.
type StatusClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// What a shown Device mesh pane follows: the server's status, read on a
/// thread of its own.
///
/// The server rewrites its metrics every second as a heartbeat, so a watch
/// that woke the window for each would be a one-second timer again. The
/// thread re-reads the status on each heartbeat and once its freshness
/// would lapse, and wakes the window only when the status changes.
struct MeshFollow {
    status: Arc<Mutex<Option<ServerRuntimeStatus>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    poke: std::sync::mpsc::Sender<()>,
}

impl MeshFollow {
    fn start(config: &ServerConfig) -> Option<Self> {
        Self::start_at(config, Arc::new(ui::time::wall))
    }

    fn start_at(config: &ServerConfig, now: StatusClock) -> Option<Self> {
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
            Box::new(move || follow_server(&config, &latest, &stop, poke, &events, &wake, &*now))
        };
        crate::device_read::thread("settings-server-status", follow).ok()?;
        Some(Self { status, stop, poke })
    }

    #[cfg(test)]
    fn by_hand() -> Self {
        Self {
            status: Arc::new(Mutex::new(None)),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            poke: std::sync::mpsc::channel().0,
        }
    }

    fn status(&self) -> Option<ServerRuntimeStatus> {
        self.status.lock().ok().and_then(|status| *status)
    }
}

impl Drop for MeshFollow {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = self.poke.send(());
    }
}

fn follow_server(
    config: &ServerConfig,
    latest: &Mutex<Option<ServerRuntimeStatus>>,
    stop: &std::sync::atomic::AtomicBool,
    poke: std::sync::mpsc::Sender<()>,
    events: &std::sync::mpsc::Receiver<()>,
    wake: &ui::WakeHandle,
    now: &(dyn Fn() -> SystemTime + Send + Sync),
) {
    use std::sync::mpsc::RecvTimeoutError;
    let heartbeat = [config
        .state_root
        .join(tilo_server_protocol::metrics::METRICS_FILE)];
    let on_change: crate::watch::OnChange = Arc::new(move || {
        let _ = poke.send(());
    });
    let mut watch = crate::watch::Watched::calling(&heartbeat, &on_change);
    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        let status = ServerRuntimeStatus::read(config, now());
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
            ServerRuntimeStatus::Offline | ServerRuntimeStatus::Unreadable
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

fn runtime_status_tone(status: ServerRuntimeStatus) -> StatusTone {
    match status {
        ServerRuntimeStatus::Online => StatusTone::Success,
        ServerRuntimeStatus::Unreadable => StatusTone::Warning,
        ServerRuntimeStatus::Offline => StatusTone::Neutral,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DeviceMeshEffect {
    /// The work this device takes from the person's other devices. A device
    /// that takes work runs services, so this asks first.
    #[effect(kind = "outside")]
    #[effect(destructive, idempotent)]
    SetWork {
        #[param("Nothing, My own devices' work, or Anyone's work in my workspaces", choices = WORK_FROM)]
        work: String,
    },
    /// Take work only while charging.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    SetWorkWhileCharging { on: bool },
    /// Keep processors or memory free.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    SetWorkRoom {
        #[param("processors or memory", choices = ROOM_PARTS)]
        part: String,
        #[param("No minimum, 20 % or 50 %")]
        percent: String,
    },
    /// Offer or stop one service of a device that takes work.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    SetDeviceService {
        #[param("inbound_server, remote_control_server, app_serving or compute", choices = SERVICES)]
        service: String,
        on: bool,
    },
    /// Print a one-use code a new computer joins with.
    #[effect(kind = "outside")]
    #[effect(label = "mesh-add-device")]
    AddDevice,
    /// Put the printed code on the clipboard, to hand to the new computer.
    #[effect(view, label = "mesh-code-copy")]
    CopyCode {
        #[param("The code as the pane prints it")]
        code: String,
    },
    /// Rename a home server.
    #[effect(kind = "outside", on = HomeServer)]
    #[effect(row)]
    RenameHomeServer {
        #[param("The home server's node id")]
        device: String,
        #[param("Its new name")]
        name: String,
    },
    /// Remove a home server; it stops serving the person's workspaces.
    #[effect(kind = "outside", on = HomeServer)]
    #[effect(row, destructive, label = "mesh-remove-home-confirm")]
    RemoveHomeServer {
        #[param("The home server's node id")]
        device: String,
    },
    /// Ask, under a device's row, whether to remove it.
    #[effect(view, row, on = HomeServer)]
    ReviewRemoval {
        #[param("The device's key or home server's node id")]
        device: String,
    },
    /// Keep the device whose removal was asked about.
    #[effect(view)]
    CancelRemoval,
}

const ROOM_PARTS: [&str; 2] = ["processors", "memory"];
const SERVICES: [&str; 4] = [
    "inbound_server",
    "remote_control_server",
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
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let model = crate::panes::device_mesh::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = model.borrow_mut();
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

    fn source(&self) -> PathBuf {
        if self.devices {
            PathBuf::from(DEFAULT_CONFIG)
        } else {
            PathBuf::from("/nonexistent/tilo/server.toml")
        }
    }

    pub fn model(&self) -> &Rc<RefCell<DeviceMeshModel>> {
        self.model.get_or_init(|| {
            let read = self
                .read
                .take()
                .unwrap_or_else(|| MeshRead::of(self.source(), ui::time::wall()));
            let mut model = DeviceMeshModel::from_read(self.devices, read);
            model.remote = self.remote.clone();
            if self.devices {
                model.follow_personal_devices();
                // The person's servers are another app's to list, and reading them asks the
                // person first: only the pane being shown asks, never a window's warm-up.
            } else if let Some(scene) = self.scene.as_deref() {
                model.personal_devices = Some(scene.personal_devices.clone());
            } else {
                model.personal_devices = Some(Vec::new());
            }
            Rc::new(RefCell::new(model))
        })
    }

    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let path = self.source();
        let now = ui::time::wall();
        self.read.start(move || MeshRead::of(path, now))
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
        model.servers.iter().map(home_server_row).collect()
    }
}

/// A home server the person's devices sync through, as the pane publishes
/// it: a row of the `settings.home-server` type, offering to show, rename
/// and remove it.
#[derive(tilo_resource::Row)]
#[row(
    kind = "settings.home-server",
    label = "Home server",
    effects = DeviceMeshEffect
)]
pub(crate) struct HomeServer;

fn home_server_row_id(node: &str) -> String {
    format!("settings.home-server:{node}")
}

fn home_server_row(peer: &tilo_server_protocol::servers::PersonServer) -> LiveRow {
    let device = peer.device.clone();
    let bindings = tilo_resource::RowBindings::new()
        .bind(
            DeviceMeshEffect::RENAME_HOME_SERVER,
            [("device", device.clone().into())],
        )
        .bind(
            DeviceMeshEffect::REVIEW_REMOVAL,
            [("device", device.clone().into())],
        )
        .bind(
            DeviceMeshEffect::REMOVE_HOME_SERVER,
            [("device", device.clone().into())],
        );
    LiveRow::typed_with::<HomeServer>(home_server_row_id(&device), peer.label.clone(), &bindings)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HomePeerHealth {
    Connecting,
    Online,
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
    /// Asking this computer for a code a new computer joins with.
    Issuing,
    /// A code is ready: it works once, for ten minutes.
    Issued,
    /// The code is on the clipboard.
    Copied,
    /// The code could not be put on the clipboard: the person copies it by hand.
    CopyFailed,
    /// This computer could not print a code.
    IssueFailed,
}

impl MeshStatus {
    /// What the pane says under the device list about a code: none while nothing is being
    /// printed or copied.
    fn ticket_shown(&self) -> Option<String> {
        Some(match self {
            Self::Issuing => t("mesh-code-issuing"),
            Self::Issued => t("mesh-code-ready"),
            Self::Copied => t("mesh-code-copied"),
            Self::CopyFailed => t("mesh-code-copy-failed"),
            Self::IssueFailed => t("mesh-code-failed"),
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
    config: ServerConfig,
    status: MeshStatus,
    devices: bool,
    changed: bool,
    runtime_status: ServerRuntimeStatus,
    /// The person's servers, as their workspace's document names them; empty until the
    /// session's first answer lands.
    servers: Vec<tilo_server_protocol::servers::PersonServer>,
    /// The session's answer to the last reading of the person's servers.
    servers_call: Option<ui::InvocationCall>,
    /// The person's root key; read from the identity when `None`.
    person: Option<[u8; 32]>,
    home_peer_health: HashMap<String, HomePeerHealth>,
    /// When each home server last answered while this pane ran: a reading it shows, which the
    /// app keeps no record of.
    home_peer_last_seen: HashMap<String, u64>,
    /// The person's devices as the session last read them; `None` until its first reading lands.
    personal_devices: Option<Vec<PersonalDevice>>,
    personal_devices_unavailable: bool,
    /// The session's reading of the person's devices, followed without waiting for it: each
    /// reading wakes the window.
    session_devices: Option<ui::collaboration::SessionResource>,
    device_rows: DisclosureGroup,
    removing: Option<String>,
    management_call: Option<(ui::InvocationCall, Managing)>,
    next_peer_health: usize,
    /// The call printing a code for a new computer, while it runs.
    ticket_call: Option<ui::InvocationCall>,
    /// The code printed here last: what the pane shows with its copy control.
    ticket: Option<String>,
    copy: ui::CopyWatch,
    remote: ui::RemoteActions,
    next_status_refresh: Instant,
    following: std::rc::Weak<MeshFollow>,
    pages: std::rc::Weak<()>,
    health_round: usize,
}

/// What the pane asked about one of the person's home servers, named by its
/// device key: each is an action on their Servers workspace, or the identity
/// step that signs what retires it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Managing {
    /// Its health (`server.status`).
    Status(String),
    /// A new name for it (`server.rename`).
    Rename(String, String),
    /// The revocation of its key the person's root signs (the identity step
    /// `revoke`), which retires it next.
    Revoke(String),
    /// Taking it out of service with that revocation (`server.retire`).
    Retire(String),
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

/// The arguments a message that counts devices names them with.
fn name_args(names: &[&str]) -> Vec<(&'static str, ui::i18n::ArgValue)> {
    let count = i64::try_from(names.len()).unwrap_or(i64::MAX);
    vec![
        ("count", count.into()),
        ("first", names.first().copied().unwrap_or_default().into()),
        ("second", names.get(1).copied().unwrap_or_default().into()),
        ("more", count.saturating_sub(2).into()),
    ]
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
        HomePeerHealth::Online => "mesh-badge-online",
        HomePeerHealth::Removed => "mesh-badge-removed",
        HomePeerHealth::Unreachable => "mesh-badge-offline",
    })
}

fn peer_health_detail(health: HomePeerHealth) -> String {
    t(match health {
        HomePeerHealth::Connecting => "mesh-health-connecting",
        HomePeerHealth::Online => "mesh-health-ready",
        HomePeerHealth::Removed => "mesh-badge-removed",
        HomePeerHealth::Unreachable => "mesh-health-unreachable",
    })
}

pub(crate) struct MeshRead {
    config_path: PathBuf,
    config: ServerConfig,
    status: MeshStatus,
    runtime_status: ServerRuntimeStatus,
    home_peer_last_seen: HashMap<String, u64>,
}

/// The session's reading of the person's devices: the one list Quick Settings shows too.
const SESSION_DEVICES: &str = "session:devices";

#[derive(serde::Deserialize)]
struct SessionDevices {
    #[serde(default)]
    devices: Vec<PersonalDevice>,
}

/// One of the person's devices, as the session reads it off the device fabric.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PersonalDevice {
    /// Its key in hex, which names it in the fabric and on its row.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    #[serde(default)]
    pub online: bool,
    /// It is heard on this device's local network.
    #[serde(default)]
    pub nearby: bool,
    /// This machine.
    #[serde(default)]
    pub local: bool,
}

/// Follow the session's reading of the person's devices. Nothing waits for it: the window is
/// woken at each reading.
pub(crate) fn follow_devices() -> Result<ui::collaboration::SessionResource, String> {
    ui::collaboration::SessionResource::follow(SESSION_DEVICES, ui::WakeHandle::new())
}

/// The newest reading `followed` brought since it was last asked; `None` when none came.
pub(crate) fn newest_devices(
    followed: &ui::collaboration::SessionResource,
) -> Option<Result<Vec<PersonalDevice>, String>> {
    let reading = followed.latest()?;
    Some(reading.and_then(|reading| {
        devices_in(&reading).ok_or_else(|| "the reading does not read".to_owned())
    }))
}

/// The person's devices `reading` lists; `None` when it does not read.
fn devices_in(reading: &str) -> Option<Vec<PersonalDevice>> {
    let reading: SessionDevices = serde_json::from_str(reading).ok()?;
    Some(
        reading
            .devices
            .into_iter()
            .map(|device| PersonalDevice {
                id: device.id.to_ascii_lowercase(),
                ..device
            })
            .collect(),
    )
}

impl MeshRead {
    fn of(config_path: PathBuf, now: SystemTime) -> Self {
        let (config, status) = match ServerConfig::load(&config_path) {
            Ok(config) => (config, MeshStatus::Ready),
            // No sync settings file: this device was never set up to sync, whether or not the
            // directory that would hold it exists.
            Err(tilo_server_protocol::config::ConfigError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                let config = ServerConfig {
                    work: WorkSetting::none(),
                    services: tilo_server_protocol::ServiceCapabilities::default(),
                    ..ServerConfig::default()
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
                (ServerConfig::default(), MeshStatus::ConfigUnreadable)
            }
        };
        let runtime_status = ServerRuntimeStatus::read(&config, now);
        Self {
            config_path,
            config,
            status,
            runtime_status,
            home_peer_last_seen: HashMap::new(),
        }
    }
}

impl DeviceMeshModel {
    #[cfg(test)]
    fn from_path(config_path: PathBuf, devices: bool) -> Self {
        Self::from_read(devices, MeshRead::of(config_path, ui::time::wall()))
    }

    fn from_read(devices: bool, read: MeshRead) -> Self {
        Self {
            config_path: read.config_path,
            config: read.config,
            status: read.status,
            devices,
            changed: false,
            runtime_status: read.runtime_status,
            servers: Vec::new(),
            servers_call: None,
            person: None,
            home_peer_health: HashMap::new(),
            home_peer_last_seen: read.home_peer_last_seen,
            personal_devices: None,
            personal_devices_unavailable: false,
            session_devices: None,
            device_rows: DisclosureGroup::new(),
            removing: None,
            management_call: None,
            next_peer_health: 0,
            ticket_call: None,
            ticket: None,
            copy: ui::CopyWatch::default(),
            remote: ui::RemoteActions::current(),
            next_status_refresh: ui::time::now() + STATUS_REFRESH_INTERVAL,
            following: std::rc::Weak::new(),
            pages: std::rc::Weak::new(),
            health_round: 0,
        }
    }

    #[cfg(test)]
    fn with_path(path: &std::path::Path) -> Self {
        let mut pane = Self::from_path(path.to_path_buf(), false);
        pane.changed = false;
        pane.next_status_refresh = ui::time::now() + Duration::from_secs(24 * 60 * 60);
        pane
    }

    fn apply(&mut self, effect: &DeviceMeshEffect) -> Result<(), String> {
        self.follow_removal(effect);
        let configured = match effect {
            DeviceMeshEffect::SetWork { work } => {
                let from =
                    work_from_key(work).ok_or_else(|| "unsupported work setting".to_string())?;
                self.apply_work(work_from(from, self.config.work.when));
                true
            }
            DeviceMeshEffect::SetWorkWhileCharging { on } => {
                self.config.work.when.charging_only = *on;
                true
            }
            DeviceMeshEffect::SetWorkRoom { part, percent } => {
                let percent = room_from_label(percent)
                    .or_else(|| percent.trim_end_matches('%').trim().parse().ok())
                    .filter(|percent| ROOM.contains(percent))
                    .ok_or_else(|| "unsupported share to keep free".to_string())?;
                match part.as_str() {
                    "processors" => self.config.work.when.free_cpu_percent = percent,
                    "memory" => self.config.work.when.free_memory_percent = percent,
                    _ => return Err("unsupported part of the machine".into()),
                }
                true
            }
            DeviceMeshEffect::SetDeviceService { service, on } => {
                match service.as_str() {
                    "inbound_server" => self.config.services.inbound_server = *on,
                    "remote_control_server" => self.config.services.remote_control_server = *on,
                    "app_serving" => self.config.services.app_serving = *on,
                    "compute" => self.config.services.compute = *on,
                    _ => return Err("unsupported device service".into()),
                }
                true
            }
            DeviceMeshEffect::AddDevice => {
                self.issue_ticket();
                false
            }
            DeviceMeshEffect::CopyCode { code } => {
                self.copy_code(code);
                false
            }
            DeviceMeshEffect::RenameHomeServer { device, name } => {
                let peer = self.home_peer(device)?;
                if !tilo_server_protocol::valid_device_label(name) {
                    return Err("that name cannot be used for a home server".into());
                }
                let managing = Managing::Rename(peer.device.clone(), name.trim().into());
                self.ask_server(&peer, "server.rename", [("name", name.trim())], managing);
                false
            }
            DeviceMeshEffect::RemoveHomeServer { device } => {
                let peer = self.home_peer(device)?;
                self.start_home_peer_removal(&peer);
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

    /// Follow the session's reading of the person's devices. Nothing waits for it here: the
    /// window is woken at each reading, and the list shows that it is being read until the first
    /// one lands.
    fn follow_personal_devices(&mut self) {
        match follow_devices() {
            Ok(followed) => self.session_devices = Some(followed),
            Err(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "the session's devices are not followed"
                );
                self.personal_devices_unavailable = true;
            }
        }
    }

    /// Take in the session's newest reading of the person's devices.
    fn take_personal_devices(&mut self) {
        let Some(reading) = self.session_devices.as_ref().and_then(newest_devices) else {
            return;
        };
        if let Ok(devices) = reading {
            self.changed |= self.personal_devices_unavailable;
            self.personal_devices_unavailable = false;
            if self.personal_devices.as_ref() != Some(&devices) {
                self.personal_devices = Some(devices);
                self.changed = true;
            }
        } else {
            tracing::warn!(
                target: "tilo::settings",
                "the session's reading of the devices does not read"
            );
            self.changed |= !self.personal_devices_unavailable;
            self.personal_devices_unavailable = true;
        }
    }

    /// Ask the session for the person's servers; the answer lands in [`Self::poll_host_calls`].
    fn read_servers(&mut self) {
        use ui::tilo_actions::remote::{InvokeArgument, machines};
        if self.servers_call.is_none() {
            self.servers_call = Some(self.remote.invoke(
                machines::target(machines::SERVERS),
                InvokeArgument::None,
                None,
            ));
        }
    }

    fn poll_host_calls(&mut self) {
        if self.management_call.is_some() || self.servers_call.is_some() {
            self.remote.poll();
        }
        if let Some(answer) = self
            .servers_call
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.servers_call = None;
            match answer.map(|output| output.value.map(serde_json::from_value)) {
                Ok(Some(Ok(servers))) => {
                    self.changed |= self.servers != servers;
                    self.servers = servers;
                }
                Ok(_) => tracing::warn!(
                    target: "tilo::settings",
                    "the session's list of the person's servers does not read"
                ),
                Err(detail) => tracing::warn!(
                    target: "tilo::settings",
                    %detail,
                    "the session did not list the person's servers"
                ),
            }
        }
        if let Some(answer) = self
            .management_call
            .as_ref()
            .and_then(|(call, _)| crate::device_fleet::settled(call))
            && let Some((_, managing)) = self.management_call.take()
        {
            self.take_management_answer(managing, answer.map(|output| output.value));
        }
    }

    /// Remove `peer`: the person's root signs the revocation of its key here,
    /// and its entry in their Servers workspace is retired with it.
    fn start_home_peer_removal(&mut self, peer: &tilo_server_protocol::servers::PersonServer) {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        if self.management_call.is_some() {
            return;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("device".into(), peer.device.clone().into());
        arguments.insert("reason".into(), "retired".into());
        arguments.insert("confirm".into(), true.into());
        let call = self.remote.invoke(
            identity_admin::target(identity_admin::REVOKE),
            InvokeArgument::Json(arguments),
            Some(Confirmation::Confirmed),
        );
        self.management_call = Some((call, Managing::Revoke(peer.device.clone())));
        self.home_peer_health
            .insert(peer.device.clone(), HomePeerHealth::Connecting);
        self.changed = true;
    }

    /// Ask the action `verb` of `peer`'s entry in its person's Servers
    /// workspace, with `arguments` beside the server it names.
    fn ask_server<const N: usize>(
        &mut self,
        peer: &tilo_server_protocol::servers::PersonServer,
        verb: &str,
        arguments: [(&str, &str); N],
        managing: Managing,
    ) {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, RemoteActionTarget};
        let Some(person) = self.person() else {
            self.home_peer_health
                .insert(peer.device.clone(), HomePeerHealth::Unreachable);
            return;
        };
        let mut argument = serde_json::Map::new();
        argument.insert("server".into(), peer.device.clone().into());
        for (name, value) in arguments {
            argument.insert(name.into(), value.into());
        }
        let confirmed =
            (!matches!(managing, Managing::Status(_))).then_some(Confirmation::Confirmed);
        let call = self.remote.invoke(
            RemoteActionTarget::workspace_governance(
                &tilo_server_protocol::servers::person_workspace_id(&person),
                verb,
            ),
            InvokeArgument::Json(argument),
            confirmed,
        );
        self.management_call = Some((call, managing));
        self.home_peer_health
            .insert(peer.device.clone(), HomePeerHealth::Connecting);
        self.changed = true;
    }

    fn take_management_answer(
        &mut self,
        managing: Managing,
        answer: Result<Option<serde_json::Value>, String>,
    ) {
        let device = match &managing {
            Managing::Status(device)
            | Managing::Rename(device, _)
            | Managing::Revoke(device)
            | Managing::Retire(device) => device.clone(),
        };
        let health = match (managing, answer) {
            (Managing::Revoke(_), Ok(answer)) => {
                let revocation = answer
                    .as_ref()
                    .and_then(|answer| answer.get("revocation"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                match (revocation, self.home_peer(&device)) {
                    (Some(revocation), Ok(peer)) => {
                        let managing = Managing::Retire(device);
                        self.ask_server(
                            &peer,
                            "server.retire",
                            [("revocation", revocation.as_str())],
                            managing,
                        );
                        return;
                    }
                    _ => HomePeerHealth::Unreachable,
                }
            }
            (Managing::Retire(_), Ok(_)) => HomePeerHealth::Removed,
            (Managing::Rename(..) | Managing::Status(_), Ok(_)) => HomePeerHealth::Online,
            (_, Err(detail)) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %detail,
                    "the home server did not answer"
                );
                HomePeerHealth::Unreachable
            }
        };
        if health != HomePeerHealth::Unreachable {
            self.home_peer_last_seen
                .insert(device.clone(), ui::time::unix_now_ms());
            self.read_servers();
        }
        self.home_peer_health.insert(device, health);
        self.changed = true;
    }

    /// The person's root key, which names their Servers workspace: the
    /// identity's, unless a test set it.
    fn person(&self) -> Option<[u8; 32]> {
        self.person.or_else(|| {
            tilo_identity_client::load_metadata()
                .inspect_err(|error| {
                    tracing::warn!(target: "tilo::settings", ?error, "the identity is not readable");
                })
                .ok()
                .map(|metadata| metadata.group_public_key)
        })
    }

    fn home_peer(
        &self,
        device: &str,
    ) -> Result<tilo_server_protocol::servers::PersonServer, String> {
        self.servers
            .iter()
            .find(|peer| peer.device == device)
            .cloned()
            .ok_or_else(|| "that home server is no longer paired".to_string())
    }

    fn refresh_runtime_status(&mut self) {
        let status = ServerRuntimeStatus::read(&self.config, ui::time::wall());
        if status != self.runtime_status {
            self.runtime_status = status;
            self.changed = true;
        }
        self.next_status_refresh = ui::time::now() + STATUS_REFRESH_INTERVAL;
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
        self.config.services.inbound_server = takes;
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
        row: &(impl ui::tilo_actions::Exports + ?Sized),
        control: C,
        effect: impl tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        crate::on_row::bind(&self.dispatcher, row, control, effect)
    }

    pub(super) fn bind_row<C: ui::framework::ActionControl<()>>(
        &self,
        control: C,
        row: &(impl ui::tilo_actions::Exports + ?Sized),
        effect: impl Into<SettingsEffect>,
    ) -> C {
        crate::on_row::bind_invocation(&self.dispatcher, row, control, effect)
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
        let others: Vec<&str> = self
            .personal_devices
            .iter()
            .flatten()
            .filter(|device| !device.local)
            .map(|device| device.label.as_str())
            .collect();

        let mut groups = vec![self.devices_section().into_element()];
        if others.is_empty() {
            let waiting = rows(vec![note(&t("mesh-share-once-added"))]);
            groups.push(section(&t("mesh-privacy-title"), waiting).into_element());
        } else {
            let shared_with = t_args("mesh-shared-with", &name_args(&others));
            groups.extend(self.continuity_group(move |cx, continuity| {
                privacy_section(cx, continuity, &shared_with)
            }));
            // Sharing history comes from other devices.
            groups.extend(self.continuity_group(|_, continuity| activity_section(continuity)));
        }
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
        use crate::prefs::McpServerDevice;
        let api = self.cx.settings::<McpServerDevice>();
        self.cx.record(&api, |cx| {
            // Letting their steps go unasked is for agents let in at all: the row under the first
            // waits for it to be on.
            cx.switch(
                &cx.value(|api: &McpServerDevice| api.accept_own_devices),
                |cx, accepting| {
                    let act = cx
                        .setting(t("mesh-agents-act"))
                        .described(t(if *accepting {
                            "mesh-agents-act-desc"
                        } else {
                            "mesh-agents-act-needs-accept"
                        }))
                        .id(AGENTS_ACT_KEY)
                        .nested()
                        .enabled(*accepting)
                        .toggle_verb(
                            |api: &McpServerDevice| api.act_without_asking,
                            McpServerDevice::SET_ACT_WITHOUT_ASKING,
                        );
                    section(
                        &t("mesh-agents-title"),
                        rows(vec![
                            cx.setting(t("mesh-agents-accept"))
                                .described(t("mesh-agents-accept-desc"))
                                .id(AGENTS_ACCEPT_KEY)
                                .toggle_verb(
                                    |api: &McpServerDevice| api.accept_own_devices,
                                    McpServerDevice::SET_ACCEPT_OWN_DEVICES,
                                ),
                            act,
                        ]),
                    )
                    .into_element()
                },
            )
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
    shared_with: &str,
) -> SettingsSection {
    let mut all = cx
        .setting(t("mesh-allow-continuity"))
        .id(CONTINUITY_ENABLED_KEY);
    if continuity.enabled {
        all = all.described(shared_with);
    }
    let mut privacy_rows = vec![all.toggle_verb(
        |continuity: &ContinuityPrivacy| continuity.enabled,
        ContinuityPrivacy::ALLOW_CONTINUITY,
    )];
    // Each kind of sharing refines the switch above, so it shows only while that is on.
    for capability in Capability::ALL.into_iter().filter(|_| continuity.enabled) {
        let facet = capability.key().to_owned();
        privacy_rows.push(
            cx.setting(continuity::label(capability))
                .described(continuity::description(capability))
                .id(continuity_key(capability))
                .toggle_verb(
                    move |continuity: &ContinuityPrivacy| continuity.allowed(capability),
                    ContinuityPrivacy::ALLOW_CAPABILITY.map(move |on: bool| (facet.clone(), on)),
                ),
        );
    }
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

    /// Ask this computer for a code a new computer joins with, one at a time.
    fn issue_ticket(&mut self) {
        use ui::tilo_actions::remote::{InvokeArgument, device_tickets};
        if self.ticket_call.is_some() {
            return;
        }
        self.ticket_call = Some(self.remote.invoke(
            device_tickets::target(device_tickets::ISSUE),
            InvokeArgument::Json(serde_json::Map::new()),
            None,
        ));
        self.status = MeshStatus::Issuing;
        self.changed = true;
    }

    /// Put the printed code on the clipboard; whether the person copied it.
    fn copy_code(&mut self, code: &str) {
        self.status = if self.copy.watch(ui::try_clipboard_copy(code)) {
            MeshStatus::Copied
        } else {
            MeshStatus::CopyFailed
        };
        self.changed = true;
    }

    /// Take the code the daemon answered, or what stopped it, once it answers.
    fn poll_ticket(&mut self) {
        use ui::tilo_actions::InvocationUpdate;
        let Some(call) = self.ticket_call.as_ref() else {
            return;
        };
        self.remote.poll();
        let answer = call.get();
        if !answer.is_finished() {
            return;
        }
        self.ticket_call = None;
        self.changed = true;
        let output = match answer {
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => output,
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                tracing::warn!(
                    target: "tilo::settings",
                    message = %error.message,
                    "a code for a new computer was not printed"
                );
                self.status = MeshStatus::IssueFailed;
                return;
            }
            ui::Arrival::Ready(_) => {
                self.status = MeshStatus::IssueFailed;
                return;
            }
            ui::Arrival::Failed(error) => {
                tracing::warn!(
                    target: "tilo::settings",
                    %error,
                    "a code for a new computer was not printed"
                );
                self.status = MeshStatus::IssueFailed;
                return;
            }
            ui::Arrival::Pending { .. } => return,
        };
        let code = output
            .value
            .as_ref()
            .and_then(|answer| answer.get("ticket"))
            .and_then(serde_json::Value::as_str);
        if let Some(code) = code {
            self.ticket = Some(code.to_owned());
            self.status = MeshStatus::Issued;
        } else {
            tracing::warn!(target: "tilo::settings", "the daemon printed no code");
            self.status = MeshStatus::IssueFailed;
        }
    }

    fn refresh_one_home_peer(&mut self) {
        if self.management_call.is_some() || self.servers.is_empty() {
            return;
        }
        let index = self.next_peer_health % self.servers.len();
        self.next_peer_health = self.next_peer_health.wrapping_add(1);
        let peer = self.servers.get(index).cloned();
        if let Some(peer) = peer {
            let managing = Managing::Status(peer.device.clone());
            self.ask_server(&peer, "server.status", [], managing);
        }
    }

    fn poll_background_tasks(&mut self) {
        self.poll_ticket();
        self.poll_host_calls();
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

    fn poll(&mut self) -> bool {
        self.poll_background_tasks();
        self.take_personal_devices();
        if let Some(follow) = self.following.upgrade() {
            self.take_followed(&follow);
        } else if ui::time::now() >= self.next_status_refresh {
            self.refresh_runtime_status();
            self.refresh_one_home_peer();
        }
        std::mem::take(&mut self.changed)
    }

    fn busy(&self) -> bool {
        self.ticket_call.is_some()
    }

    fn next_wake(&self) -> Option<Instant> {
        if !takes_work(&self.config.work) {
            return None;
        }
        match self.following.upgrade() {
            Some(_) => (!self.servers.is_empty()).then_some(self.next_status_refresh),
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
        let follow = Rc::new(MeshFollow::start(&self.config)?);
        self.following = Rc::downgrade(&follow);
        self.health_round = self.servers.len().max(1);
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
    // The pane is shown: ask for the person's servers now, where the question about another
    // app's action belongs.
    if held.devices {
        model.borrow_mut().read_servers();
    }
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
        if self.management_call.is_none() && (due || self.health_round > 0) {
            self.health_round = self.health_round.saturating_sub(1);
            self.next_status_refresh = now + HOME_PEER_HEALTH_INTERVAL;
            self.refresh_one_home_peer();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    tilo_test_env::scenario! {
        fn the_device_mesh_model_reads_and_edits_the_server_configuration;

        phase a_device_without_sync_settings_is_unconfigured_not_unreadable;
        {
            let dir = tempfile::tempdir().unwrap();
            let absent = dir.path().join("etc-tilo").join("server.toml");
            let pane = DeviceMeshModel::with_path(&absent);
            assert_eq!(pane.status, MeshStatus::NotConfigured);
            assert!(pane.status.settings_trouble_shown().is_none());
            let config = ServerConfig {
                state_root: dir.path().join("missing").join("tilo-server"),
                ..ServerConfig::default()
            };
            assert_eq!(
                ServerRuntimeStatus::read(&config, ui::time::wall()),
                ServerRuntimeStatus::Unreadable
            );
            let config = ServerConfig {
                state_root: dir.path().join("tilo-server"),
                ..ServerConfig::default()
            };
            assert_eq!(
                ServerRuntimeStatus::read(&config, ui::time::wall()),
                ServerRuntimeStatus::Offline
            );
        }

        phase a_followed_pane_reads_on_a_change_and_sets_no_deadline;
        {
            let dir = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_IDENTITY_ROOT", dir.path());
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
            pane.config.work = WorkSetting::own_person();
            let follow = Rc::new(MeshFollow::by_hand());
            pane.following = Rc::downgrade(&follow);
            let _ = pane.poll();
            assert_eq!(pane.next_wake(), None, "an idle pane sets no deadline");

            *follow.status.lock().unwrap() = Some(ServerRuntimeStatus::Online);
            assert!(pane.poll(), "the thread's status is shown");
            assert_eq!(pane.runtime_status, ServerRuntimeStatus::Online);
            assert!(!pane.poll(), "once");
        }

        phase server_config_edits_are_real_honest_and_coalesced;
        {
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("server.toml");
                ServerConfig::default().save(&path).unwrap();

                let mut pane = DeviceMeshModel::with_path(&path);
                let _ = pane.poll();
                pane.apply(&DeviceMeshEffect::SetWork {
                    work: "mesh-work-anyone".into(),
                })
                .unwrap();
                pane.apply(&DeviceMeshEffect::SetDeviceService {
                    service: "compute".into(),
                    on: true,
                })
                .unwrap();
                pane.apply(&DeviceMeshEffect::SetDeviceService {
                    service: "remote_control_server".into(),
                    on: false,
                })
                .unwrap();

                let saved = ServerConfig::load(&path).unwrap();
                assert_eq!(saved.work.from, WorkFrom::Anyone);
                assert!(saved.services.compute);
                assert!(!saved.services.remote_control_server);
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("server.toml");
                let mut pane = DeviceMeshModel::with_path(&path);
                assert_eq!(pane.status, MeshStatus::NotConfigured);
                assert!(pane.config.work.takes_nothing());
                assert_eq!(
                    pane.config.services,
                    tilo_server_protocol::ServiceCapabilities::default()
                );
                let _ = pane.poll();
                assert!(!path.exists());
                pane.apply(&DeviceMeshEffect::SetWork {
                    work: "mesh-work-own".into(),
                })
                .unwrap();
                assert!(path.exists());
            }

            {
                // A choice is a message key, so a language's label is never one.
                assert_eq!(work_from_key("mesh-work-nothing"), Some(WorkFrom::Nobody));
                assert_eq!(work_from_key("mesh-work-own"), Some(WorkFrom::OwnDevices));
                assert_eq!(work_from_key("mesh-work-anyone"), Some(WorkFrom::Anyone));
                assert_eq!(work_from_key(&t("mesh-work-some")), None);
                assert!(work_from(WorkFrom::Nobody, WorkWhen::always()).takes_nothing());
                assert!(!work_from(WorkFrom::OwnDevices, WorkWhen::always()).takes_nothing());
            }
        }

        phase the_states_a_device_takes_work_in_are_kept;
        // The states a device takes work in are saved as the page sets them, and picking whose work
        // it takes keeps them.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("server.toml");
            ServerConfig::default().save(&path).unwrap();
            let mut pane = DeviceMeshModel::with_path(&path);
            let _ = pane.poll();

            pane.apply(&DeviceMeshEffect::SetWorkWhileCharging { on: true })
                .unwrap();
            pane.apply(&DeviceMeshEffect::SetWorkRoom {
                part: "processors".into(),
                percent: room_shown(20),
            })
            .unwrap();
            pane.apply(&DeviceMeshEffect::SetWorkRoom {
                part: "memory".into(),
                percent: "50".into(),
            })
            .unwrap();
            assert!(
                pane.apply(&DeviceMeshEffect::SetWorkRoom {
                    part: "memory".into(),
                    percent: "35".into(),
                })
                .is_err(),
                "only the shares the page offers are kept"
            );
            pane.apply(&DeviceMeshEffect::SetWork {
                work: "mesh-work-own".into(),
            })
            .unwrap();
            let saved = ServerConfig::load(&path).unwrap();
            assert_eq!(
                saved.work.when,
                WorkWhen {
                    charging_only: true,
                    free_cpu_percent: 20,
                    free_memory_percent: 50,
                },
                "whose work a device takes leaves the states it takes it in"
            );
        }

        phase the_work_a_device_takes_and_its_services_stay_local;
        {
            {
                assert!(!takes_work(&WorkSetting::none()));
                assert!(takes_work(&WorkSetting::own_person()));
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("server.toml");
                let config = ServerConfig {
                    work: WorkSetting::none(),
                    ..ServerConfig::default()
                };
                config.save(&path).unwrap();
                let mut pane = DeviceMeshModel::with_path(&path);
                let _ = pane.poll();
                assert!(!pane.poll());

                pane.apply(&DeviceMeshEffect::SetWork {
                    work: "mesh-work-own".into(),
                })
                .unwrap();

                assert!(pane.poll());
                assert_eq!(pane.config.work.from, WorkFrom::OwnDevices);
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("server.toml");
                let config = ServerConfig {
                    work: WorkSetting::none(),
                    services: tilo_server_protocol::ServiceCapabilities::default(),
                    ..ServerConfig::default()
                };
                config.save(&path).unwrap();

                let mut pane = DeviceMeshModel::with_path(&path);
                let _ = pane.poll();
                pane.apply(&DeviceMeshEffect::SetWork {
                    work: "mesh-work-anyone".into(),
                })
                .unwrap();
                let saved = ServerConfig::load(&path).unwrap();
                assert_eq!(saved.work.from, WorkFrom::Anyone);
                assert!(saved.services.workspace_replication);
                assert!(saved.services.inbound_server);
            }
        }

        phase device_health_and_workspace_status_reflect_fresh_facts;
        {
            {
                assert_eq!(peer_health_badge(HomePeerHealth::Online), "Online");
                assert_eq!(peer_health_badge(HomePeerHealth::Unreachable), "Offline");
                assert_eq!(peer_health_badge(HomePeerHealth::Connecting), "Checking");
                assert_eq!(
                    peer_health_detail(HomePeerHealth::Unreachable),
                    "Offline. Check that it is on and connected"
                );
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let config = ServerConfig {
                    state_root: dir.path().to_path_buf(),
                    work: WorkSetting::own_person(),
                    ..ServerConfig::default()
                };
                let metrics = tilo_server_protocol::metrics::MetricsSnapshot {
                    deltas_received: 42,
                    ..Default::default()
                };
                tilo_server_protocol::metrics::save_metrics(&config.state_root, &metrics).unwrap();
                let modified = std::fs::metadata(
                    config
                        .state_root
                        .join(tilo_server_protocol::metrics::METRICS_FILE),
                )
                .unwrap()
                .modified()
                .unwrap();

                assert_eq!(
                    ServerRuntimeStatus::read(&config, modified + Duration::from_secs(1)),
                    ServerRuntimeStatus::Online
                );
                assert_eq!(
                    ServerRuntimeStatus::read(
                        &config,
                        modified + STATUS_FRESHNESS + Duration::from_secs(1)
                    ),
                    ServerRuntimeStatus::Offline
                );
            }
        }

        phase the_area_builds_its_model_from_the_read_it_started;
        {
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
                PathBuf::from("/nonexistent/tilo/server.toml"),
                "a test reads the stand-in configuration"
            );
            assert_eq!(model.personal_devices, Some(Vec::new()));
            assert!(model.servers.is_empty());
            drop(model);
            assert!(!area.start_read(), "a built model is not read again");
        }

        phase a_read_builds_the_model_reading_in_place_builds;
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("server.toml");
            std::fs::write(&path, "iroh_listen = \"0.0.0.0:1234\"\n").unwrap();
            let here = DeviceMeshModel::from_path(path.clone(), false);
            let read = MeshRead::of(path, ui::time::wall());
            let there = DeviceMeshModel::from_read(false, read);
            assert_eq!(here.config_path, there.config_path);
            assert_eq!(here.status, there.status);
            assert_eq!(here.config.work, there.config.work);
            assert_eq!(here.runtime_status, there.runtime_status);
        }
    }

    #[test]
    fn the_server_status_follows_its_heartbeat_and_its_lapse() {
        let held = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = {
            let held = Arc::clone(&held);
            Arc::new(move || {
                SystemTime::UNIX_EPOCH
                    + Duration::from_millis(held.load(std::sync::atomic::Ordering::Acquire))
            }) as StatusClock
        };
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("tilo-server");
        std::fs::create_dir_all(&state_root).unwrap();
        let config = ServerConfig {
            state_root,
            work: WorkSetting::own_person(),
            ..ServerConfig::default()
        };
        let follow = MeshFollow::start_at(&config, clock).expect("the status thread starts");
        tilo_test_env::wait_until("the first read", || {
            follow.status() == Some(ServerRuntimeStatus::Offline)
        });
        let metrics = tilo_server_protocol::metrics::MetricsSnapshot::default();
        tilo_server_protocol::metrics::save_metrics(&config.state_root, &metrics).unwrap();
        let written = std::fs::metadata(
            config
                .state_root
                .join(tilo_server_protocol::metrics::METRICS_FILE),
        )
        .and_then(|metadata| metadata.modified())
        .unwrap();
        held.store(
            millis_since_epoch(written),
            std::sync::atomic::Ordering::Release,
        );
        tilo_test_env::wait_until("the heartbeat to show the server online", || {
            follow.status() == Some(ServerRuntimeStatus::Online)
        });
        held.store(
            millis_since_epoch(written + STATUS_FRESHNESS + Duration::from_secs(1)),
            std::sync::atomic::Ordering::Release,
        );
        tilo_test_env::wait_until("the lapsed heartbeat to show it offline", || {
            follow.status() == Some(ServerRuntimeStatus::Offline)
        });
    }

    #[test]
    fn continuity_controls_are_typed_and_audited() {
        {
            let dir = tempfile::tempdir().unwrap();
            let mut app = window_on(devices_and_a_home_server(dir.path()));
            let ids = test_ids(app.harness().root_widget());

            assert!(ids.iter().any(|id| id == CONTINUITY_ENABLED_KEY));
            let described = descriptions(app.harness().root_widget());
            assert!(
                described.contains("On for Ada's Phone and Kitchen NAS\n"),
                "{described}"
            );
            for capability in Capability::ALL {
                let key = continuity_key(capability);
                assert!(ids.iter().any(|id| id == &key), "missing {key}");
            }
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
    fn a_code_that_was_not_printed_says_what_to_do_not_the_hosts_words() {
        const HOST_WORDS: &str = "the daemon's ticket store was busy";
        let dir = tempfile::tempdir().unwrap();
        let mut model = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
        model.remote = ui::RemoteActions::with_provider(
            Arc::new(StubHost {
                ticket_failure: Some(HOST_WORDS.into()),
                ..StubHost::default()
            }),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );
        model.apply(&DeviceMeshEffect::AddDevice).unwrap();
        model.poll_background_tasks();
        assert_eq!(model.status, MeshStatus::IssueFailed);

        let mut app = window_on(model);
        app.settle();
        let shown =
            provider_settings_ui::widgets::announced_text(app.harness().root_widget()).join("\n");
        assert!(
            shown.contains(&t("mesh-code-failed")),
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
        let mut model = devices_and_a_home_server(dir.path());
        model.status = MeshStatus::NotSaved;
        model.personal_devices_unavailable = true;
        let mut app = window_on(model);
        app.settle();
        let shown =
            provider_settings_ui::widgets::announced_text(app.harness().root_widget()).join("\n");
        for message in [
            t("mesh-status-not-saved"),
            t("mesh-status-device-list-unavailable"),
        ] {
            assert!(shown.contains(&message), "{message} is shown: {shown}");
        }
        assert!(MeshStatus::Ready.settings_trouble_shown().is_none());
        assert!(MeshStatus::NotConfigured.ticket_shown().is_none());
        assert_eq!(
            MeshStatus::ConfigUnreadable.settings_trouble_shown(),
            Some(t("mesh-status-config-unreadable"))
        );
        assert_eq!(
            MeshStatus::Copied.ticket_shown(),
            Some(t("mesh-code-copied"))
        );
    }

    /// What a reader hears after each name, one line each.
    fn descriptions(widget: &dyn Widget) -> String {
        let own = widget.accessibility().and_then(|node| node.description);
        let below = widget.children().into_iter().map(descriptions);
        own.map(|text| text + "\n")
            .into_iter()
            .chain(below)
            .collect()
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
        let mut pane = DeviceMeshModel::with_path(&dir.join("server.toml"));
        pane.personal_devices = devices_in(
            &serde_json::json!({"devices": [
                {"id": THIS_DEVICE, "label": "Ada's Laptop", "local": true, "online": true},
                {"id": PHONE, "label": "Ada's Phone"},
                {"id": NEARBY_DEVICE, "label": "Kitchen NAS", "online": true},
            ]})
            .to_string(),
        );
        pane.servers = vec![nearby_server()];
        pane
    }

    /// A device the session hears on this device's network says so on its row; the others keep
    /// what they are.
    #[test]
    fn a_device_the_session_hears_nearby_says_so() {
        let _settled = ui::motion::testing::settled();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = devices_and_a_home_server(dir.path());
        pane.personal_devices = devices_in(
            &serde_json::json!({"devices": [
                {"id": THIS_DEVICE, "label": "Ada's Laptop", "local": true, "nearby": false},
                {"id": PHONE.to_ascii_uppercase(), "label": "Ada's Phone", "nearby": true},
            ]})
            .to_string(),
        );
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
            change_model(&mut app, |pane| pane
                .personal_devices
                .iter()
                .flatten()
                .count()),
            3
        );
    }

    tilo_test_env::scenario! {
        fn adding_a_device_and_removal_run_through_the_host;

        phase device_removal_asks_first_and_printing_a_code_is_single_flight;
        {
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
                let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
                assert!(pane.apply(&effect).is_err(), "nothing unpaired is touched");
                assert!(pane.management_call.is_none());
            }

            {
                let dir = tempfile::tempdir().unwrap();
                let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
                let host = Arc::new(StubHost::default());
                pane.remote = ui::RemoteActions::with_provider(
                    host.clone(),
                    ui::tilo_actions::remote::Caller::local_person("owner"),
                );
                pane.issue_ticket();
                pane.issue_ticket();
                assert_eq!(host.asked().len(), 1, "one code at a time");
            }
        }

        phase a_server_removal_signs_its_revocation_and_retires_it;
        {
            use ui::tilo_actions::remote::{
                Confirmation, InvokeArgument, RemoteActionTarget, identity_admin, machines,
            };
            let dir = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_IDENTITY_ROOT", dir.path());
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
            let host = Arc::new(StubHost::default());
            pane.remote = ui::RemoteActions::with_provider(
                host.clone(),
                ui::tilo_actions::remote::Caller::local_person("owner"),
            );
            let server = nearby_server();
            pane.servers = vec![server];
            pane.person = Some([0x33; 32]);
            pane.apply(&DeviceMeshEffect::RemoveHomeServer {
                device: NEARBY_DEVICE.into(),
            })
            .unwrap();
            // The root signs the revocation, and the server's entry in the
            // person's Servers workspace is retired with it.
            pane.poll_background_tasks();
            pane.poll_background_tasks();
            // A revocation, the retirement, and the person's servers read again to show it gone.
            let asked = host.asked();
            assert_eq!(asked.len(), 3, "{asked:?}");
            let retired = RemoteActionTarget::workspace_governance(
                &tilo_server_protocol::servers::person_workspace_id(&[0x33; 32]),
                "server.retire",
            );
            let revoke = asked
                .iter()
                .find(|call| call.target == identity_admin::target(identity_admin::REVOKE))
                .expect("the root signs a revocation");
            let retire = asked
                .iter()
                .find(|call| call.target == retired)
                .expect("the server's entry is retired");
            assert!(
                asked
                    .iter()
                    .any(|call| call.target == machines::target(machines::SERVERS)),
                "the servers are read again: {asked:?}"
            );
            assert_eq!(revoke.confirmation, Some(Confirmation::Confirmed));
            assert_eq!(
                revoke.argument,
                InvokeArgument::Json(
                    serde_json::json!({
                        "device": NEARBY_DEVICE,
                        "reason": "retired",
                        "confirm": true,
                    })
                    .as_object()
                    .unwrap()
                    .clone()
                )
            );
            assert_eq!(retire.confirmation, Some(Confirmation::Confirmed));
            assert_eq!(
                retire.argument,
                InvokeArgument::Json(
                    serde_json::json!({
                        "server": NEARBY_DEVICE,
                        "revocation": SIGNED_REVOCATION,
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

        phase a_home_server_that_does_not_answer_is_offline_not_the_hosts_words;
        {
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
            pane.take_management_answer(
                Managing::Status(NEARBY_DEVICE.into()),
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

        phase a_code_is_printed_through_the_host_action_and_spawns_nothing;
        {
            use ui::tilo_actions::remote::{InvokeArgument, device_tickets};
            let dir = tempfile::tempdir().unwrap();
            let mut pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
            let host = Arc::new(StubHost::default());
            pane.remote = ui::RemoteActions::with_provider(
                host.clone(),
                ui::tilo_actions::remote::Caller::local_person("owner"),
            );

            pane.apply(&DeviceMeshEffect::AddDevice).unwrap();
            assert_eq!(pane.status, MeshStatus::Issuing);
            pane.poll_background_tasks();
            let asked = host.asked();
            assert_eq!(asked.len(), 1);
            assert_eq!(asked[0].target, device_tickets::target(device_tickets::ISSUE));
            assert_eq!(
                asked[0].argument,
                InvokeArgument::Json(serde_json::Map::new())
            );
            assert_eq!(pane.status, MeshStatus::Issued);
            assert_eq!(pane.ticket.as_deref(), Some(TICKET));
            assert!(pane.ticket_call.is_none(), "the code has answered");
            assert!(
                pane.management_call.is_none(),
                "printing a code started no process in the app"
            );

            // A refusal says what to do, and the code printed before still stands.
            pane.remote = ui::RemoteActions::with_provider(
                Arc::new(StubHost {
                    ticket_failure: Some("no identity here".into()),
                    ..StubHost::default()
                }),
                ui::tilo_actions::remote::Caller::local_person("owner"),
            );
            pane.apply(&DeviceMeshEffect::AddDevice).unwrap();
            pane.poll_background_tasks();
            assert_eq!(pane.status, MeshStatus::IssueFailed);
            assert_eq!(pane.ticket.as_deref(), Some(TICKET));

            // Nothing in this process answers for a clipboard, and the pane says the copy did
            // not happen rather than claiming it did.
            pane.apply(&DeviceMeshEffect::CopyCode {
                code: TICKET.into(),
            })
            .unwrap();
            assert_eq!(pane.status, MeshStatus::CopyFailed);

            // The code is on the page, with the one control that copies it.
            let mut app = window_on(pane);
            app.settle();
            let ids = test_ids(app.harness().root_widget());
            assert!(ids.iter().any(|id| id == "device_mesh.copy_code"), "{ids:?}");
            assert!(ids.iter().any(|id| id == "device_mesh.add_device"), "{ids:?}");
        }
    }

    const NEARBY_DEVICE: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const THIS_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const PHONE: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    /// The revocation the identity step answers, as the pane passes it on.
    const SIGNED_REVOCATION: &str = "{\"signed\":true}";

    /// The server the person's workspace names for the nearby device.
    fn nearby_server() -> tilo_server_protocol::servers::PersonServer {
        serde_json::from_value(serde_json::json!({
            "device": NEARBY_DEVICE,
            "label": "Kitchen NAS",
            "endpoint": {
                // The ed25519 base point: a key that parses, which nothing here signs with.
                "id": "5866666666666666666666666666666666666666666666666666666666666666",
                "addrs": [],
            },
        }))
        .unwrap()
    }

    /// The code the stand-in daemon prints.
    const TICKET: &str = "tilo1aa0000000000000000000000000000000000000000000000000000000000000101";

    #[derive(Default)]
    pub(super) struct StubHost {
        asked: Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
        ticket_failure: Option<String>,
    }

    impl StubHost {
        pub(super) fn asked(&self) -> Vec<ui::tilo_actions::remote::InvokeRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl ui::tilo_actions::remote::ActionProvider for StubHost {
        fn query(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: ui::tilo_actions::remote::CatalogueQuery,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::CataloguePage,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            ui::tilo_actions::remote::Pending::ready(Err(
                ui::tilo_actions::remote::ActionCallError::NotPermitted,
            ))
        }

        fn invoke(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            request: ui::tilo_actions::remote::InvokeRequest,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::InvocationReceipt,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            use ui::tilo_actions::remote::device_tickets;
            let invocation = request.invocation;
            let is_ticket = request.target.provider == device_tickets::PROVIDER;
            let value = if is_ticket {
                serde_json::json!({ "ticket": TICKET })
            } else {
                serde_json::json!({
                    "device": PHONE,
                    "revoked": true,
                    "revocation": SIGNED_REVOCATION,
                })
            };
            let failure = self.ticket_failure.clone().filter(|_| is_ticket);
            self.asked.lock().unwrap().push(request);
            let status = match failure {
                Some(message) => ui::tilo_actions::remote::ReceiptStatus::Failed {
                    error: ui::tilo_actions::ActionFailure {
                        code: "device-ticket-refused".into(),
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
        ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionCallError>>
        {
            ui::tilo_actions::remote::Pending::ready(Ok(()))
        }
    }

    /// The list is the session's reading of the person's devices: until it lands the list says it
    /// is being read, and a reading that arrives later is drawn without anyone asking again.
    #[test]
    fn the_device_list_is_the_sessions_reading() {
        let _settled = ui::motion::testing::settled();
        let dir = tempfile::tempdir().unwrap();
        let pane = DeviceMeshModel::with_path(&dir.path().join("server.toml"));
        assert_eq!(pane.personal_devices, None, "nothing is read in place");
        let mut app = window_on(pane);
        let text = |app: &mut TestApp<SettingsShell>| {
            provider_settings_ui::widgets::announced_text(app.harness().root_widget()).join("\n")
        };
        let alone = text(&mut app);
        assert!(alone.contains(&t("mesh-status-devices-reading")), "{alone}");
        // With no other device nothing is shared, so no sharing switch shows.
        assert!(
            alone.contains("Add a device to choose what it shares."),
            "{alone}"
        );
        assert!(!alone.contains("Clipboard"), "{alone}");
        // Sharing history comes from other devices too.
        assert!(!alone.contains(&t("mesh-activity-title")), "{alone}");
        assert!(!alone.contains(&t("mesh-audit-empty")), "{alone}");

        let devices = devices_in(
            &serde_json::json!({"devices": [
                {"id": THIS_DEVICE, "label": "This laptop", "local": true, "online": true},
                {"id": PHONE, "label": "Phone", "online": true, "nearby": true},
            ]})
            .to_string(),
        );
        change_model(&mut app, move |pane| {
            pane.personal_devices = devices;
            pane.changed = true;
        });
        let shown = text(&mut app);
        assert!(
            !shown.contains(&t("mesh-status-devices-reading")),
            "{shown}"
        );
        assert!(shown.contains("This laptop, This device"), "{shown}");
        assert!(shown.contains("Phone, Nearby"), "{shown}");
        let described = descriptions(app.harness().root_widget());
        assert!(described.contains("On for Phone\n"), "{described}");
        assert!(!shown.contains("Add a device to choose"), "{shown}");
        assert!(shown.contains(&t("mesh-activity-title")), "{shown}");
        assert_eq!(devices_in("not a reading"), None);
    }

    fn until_finished(finished: impl Fn() -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !finished() {
            assert!(std::time::Instant::now() < deadline, "the read ends");
            std::thread::yield_now();
        }
    }
}
