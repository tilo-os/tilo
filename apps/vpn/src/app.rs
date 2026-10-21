//! Resource-native VPN runtime with the retained unified connection views.
use crate::config::{self, CommandPlan};
use crate::device::{DeviceFacts, VpnDevice};
use crate::effects::{self, VpnEffect};
use crate::import::{self, Import, Imported};
use crate::model::{ConnId, ConnKind, ConnStatus, Connection, LocalProfileDetails, Profile, Vault};
use crate::net::Transfer;
use crate::probe::{self, Prober};
use crate::resource::Vpn;
use crate::view::{
    self, DetailTab, Intent, Intents, MainAction, Notice, NoticeKind, Rates, ToolbarAction,
    ToolbarActionSlot,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use ui::WakeHandle;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, RunMode, Setting,
    SurfaceCx,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Invoke;
use ui::time::Instant;
use ui::widget::{AppModel, Column, ModalDialog, ModelPath, NavPane, NavShell, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};
/// One byte-counter reading of a connected tunnel, timestamped so a rate can
/// be derived from consecutive samples. Ephemeral live telemetry; it is not
/// persisted to the CRDT.
#[derive(Clone, Copy, Debug)]
struct RateSample {
    at: Instant,
    xfer: Transfer,
}

/// Maximum number of measured rate points plotted across the throughput
/// chart.
const THROUGHPUT_SAMPLES: usize = 56;

/// Rolling window of transfer readings for one tunnel.
#[derive(Default)]
struct TransferHistory {
    samples: Vec<RateSample>,
}

impl TransferHistory {
    /// Minimum spacing between kept samples (throttles the per-frame poll).
    const MIN_DT: f64 = 0.5;

    fn push(&mut self, xfer: Transfer) {
        let now = ui::time::now();
        if let Some(last) = self.samples.last()
            && now.duration_since(last.at).as_secs_f64() < Self::MIN_DT
        {
            return;
        }
        self.samples.push(RateSample { at: now, xfer });
        // Keep enough points to fill the chart, plus one for the leading edge.
        let cap = THROUGHPUT_SAMPLES + 1;
        if self.samples.len() > cap {
            let drop = self.samples.len() - cap;
            self.samples.drain(0..drop);
        }
    }

    /// Per-interval download/upload rate series in bytes/sec, one point per
    /// consecutive pair of readings. Empty until at least two samples exist.
    fn rate_series(&self) -> (Vec<f64>, Vec<f64>) {
        let mut down = Vec::new();
        let mut up = Vec::new();
        for w in self.samples.windows(2) {
            // `.windows(2)` always yields exactly two elements, so the `else`
            // is unreachable in practice — destructuring documents that
            // instead of indexing the window slice directly.
            let [a, b] = w else { continue };
            let dt = b.at.duration_since(a.at).as_secs_f64();
            if dt <= 0.0 {
                continue;
            }
            // Counters are monotonic; guard a reset (new tunnel) as zero.
            let d = b.xfer.rx.saturating_sub(a.xfer.rx) as f64 / dt;
            let u = b.xfer.tx.saturating_sub(a.xfer.tx) as f64 / dt;
            down.push(d);
            up.push(u);
        }
        (down, up)
    }

    /// Latest cumulative counters, if any real reading has been taken.
    fn latest(&self) -> Option<Transfer> {
        self.samples.last().map(|s| s.xfer)
    }
}
/// What the confirm dialog is about to do once the user accepts.
#[derive(Clone, Debug)]
enum Pending {
    Connect(ConnId, CommandPlan),
    Disconnect(ConnId, CommandPlan),
    TorGlobal(bool, CommandPlan),
}

/// Modal overlay state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Modal {
    None,
    Add,
    Confirm,
}

#[derive(Clone, Debug, PartialEq)]
struct ListKey {
    rows: Vec<(Connection, Option<String>, bool)>,
    selected: Option<ConnId>,
    tor_global: bool,
    rev: u64,
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "capture, routing, imports and pending rebuilds are orthogonal state"
)]
pub struct VpnApp {
    shell: NavShell,
    intents: Intents,
    main_action: ToolbarActionSlot,
    dialog: Option<ModalDialog<Column>>,
    dialog_key: Option<String>,
    dialog_changed: bool,
    list_key: Option<ListKey>,
    detail_key: Option<String>,
    list_offset: Rc<Cell<(f32, f32)>>,
    detail_offset: Rc<Cell<(f32, f32)>>,
    view_rev: u64,
    dirty: bool,
    painted_ground: Option<vello::peniko::Color>,
    vault: Vault,
    /// The fleet as the document holds it, before this device's live state
    /// is laid over it.
    shared: Rc<Vault>,
    /// Per-window selection is presentation state, not shared configuration.
    local_selected: Option<ConnId>,
    detail_tab: DetailTab,
    modal: Modal,
    /// The live confirm dialog (shared [`ModalDialog`]), built lazily while
    /// `modal == Modal::Confirm` and torn down when it resolves.
    add_kind: ConnKind,
    pending: Option<Pending>,
    /// Most recent action results, newest last. Starts empty — populated
    /// only by real user actions (imports, connect attempts).
    activity: Vec<String>,
    /// A file import or a local setup is running.
    importing: bool,
    /// The Tor service is being started or stopped.
    tor_changing: bool,
    /// A local profile is being deleted.
    deleting: Option<ConnId>,
    /// Which protocol the last import was requested for, so a failed one
    /// can be retried as the same protocol.
    import_hint: ConnKind,
    /// Existing shared profile being set up on this device. `None` means a
    /// new import creates secret-free collaborative metadata after NM
    /// succeeds.
    provision_target: Option<ConnId>,
    operation_error: Option<String>,
    /// Import errors can be retried without making the user rediscover which
    /// protocol they were importing. Other machine-action errors deliberately
    /// do not get a blind retry: the user can inspect the profile and choose
    /// Connect again after addressing the reported cause.
    retry_import_kind: Option<ConnKind>,
    /// Real, ephemeral throughput readings per connected tunnel (never in the
    /// CRDT). Populated only from live kernel counters when execution is
    /// allowed and the tunnel is actually up.
    throughput: HashMap<ConnId, TransferHistory>,
    /// Authoritative machine-local tunnel state. It is intentionally not
    /// written into the collaborative profile document.
    live_status: HashMap<ConnId, ConnStatus>,
    /// IDs whose credential-bearing `NetworkManager` profile is available on
    /// this machine. Never synchronized, serialized, indexed or rendered as
    /// tunnel detail: it is only a local activation gate.
    local_credentials: HashSet<ConnId>,
    /// Parsed display facts for profiles provisioned on this device, kept in
    /// the device-local [`VpnDevice`] setting; `NetworkManager` remains the
    /// credential/config owner.
    local_details: HashMap<ConnId, LocalProfileDetails>,
    /// Profiles whose connect or disconnect is running.
    transitions: HashSet<ConnId>,
    /// Whether this device routes all traffic over Tor.
    tor_global: bool,
    /// Reads the tunnels' real state off the UI thread.
    prober: Prober,
    /// Where edits and effects go; `None` until the framework attaches it.
    dispatcher: Option<Dispatcher<Vpn>>,
    /// This device's VPN state; `None` outside a live window.
    device: Option<Setting<VpnDevice>>,
    mode: RunMode,
    waker: WakeHandle,
    /// One-shot context supplied by the shell's typed tool service. It is
    /// consumed only after the persisted vault has loaded, so a profile route
    /// can select the real NetworkManager-backed connection by name.
    pending_tool_intent: Option<os_actions::ToolIntent>,
    /// Per-window compact-detail scroll. This is presentation state and is
    /// never synchronized with the shared profile document.
    /// A capture shows the import notice as if a file were being chosen.
    shot_loading: bool,
}

impl VpnApp {
    /// The app before the framework attaches it: no profiles until the
    /// document is presented, and nothing of this device until it knows it
    /// runs live.
    pub fn new() -> Self {
        let intents = Intents::default();
        let main_action = ToolbarActionSlot::default();
        VpnApp {
            shell: Self::new_shell(&intents, &main_action),
            intents,
            main_action,
            dialog: None,
            dialog_key: None,
            dialog_changed: false,
            list_key: None,
            detail_key: None,
            list_offset: Rc::new(Cell::new((0.0, 0.0))),
            detail_offset: Rc::new(Cell::new((0.0, 0.0))),
            view_rev: 0,
            dirty: true,
            painted_ground: None,
            vault: Vault::default(),
            shared: Rc::default(),
            local_selected: None,
            detail_tab: DetailTab::Status,
            modal: Modal::None,
            add_kind: ConnKind::WireGuard,
            pending: None,
            activity: Vec::new(),
            importing: false,
            tor_changing: false,
            deleting: None,
            import_hint: ConnKind::WireGuard,
            provision_target: None,
            operation_error: None,
            retry_import_kind: None,
            throughput: HashMap::new(),
            live_status: HashMap::new(),
            local_credentials: HashSet::new(),
            local_details: HashMap::new(),
            transitions: HashSet::new(),
            tor_global: false,
            prober: Prober::default(),
            dispatcher: None,
            device: None,
            mode: RunMode::Past,
            waker: WakeHandle::new(),
            pending_tool_intent: None,
            shot_loading: false,
        }
    }

    /// Take the dispatcher, and this device as the run mode allows: the
    /// device-local setting and `NetworkManager` in a live window, the
    /// scenario's stand-in device in a capture, nothing in a past version.
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Vpn>) {
        self.dispatcher = Some(cx.dispatcher());
        self.mode = cx.mode();
        self.waker = cx.waker();
        match self.mode {
            RunMode::Live => {
                let device = cx.settings::<VpnDevice>();
                let stored = device.get();
                self.local_details = stored.profiles;
                self.tor_global = stored.tor_global;
                self.device = Some(device);
                self.pending_tool_intent = os_actions::tool_intent_from_env();
            }
            RunMode::Capture => {
                if let Some(facts) = cx.fixture::<DeviceFacts>() {
                    self.apply_facts(&facts);
                }
            }
            RunMode::Past => {}
        }
    }

    /// Show `facts` as this device's state.
    fn apply_facts(&mut self, facts: &DeviceFacts) {
        self.local_details.clone_from(&facts.profiles);
        self.local_credentials = facts.profiles.keys().copied().collect();
        self.live_status.clone_from(&facts.status);
        self.refresh();
    }

    /// The fleet the document holds now.
    pub(crate) fn present_vault(&mut self, state: &Rc<Vault>) {
        self.shared = Rc::clone(state);
        self.refresh();
    }

    fn apply_pending_tool_intent(&mut self) {
        let Some(intent) = self.pending_tool_intent.take() else {
            return;
        };
        let os_actions::ToolIntent::Vpn { route, profile } = intent else {
            return;
        };
        match route {
            os_actions::VpnRoute::Overview | os_actions::VpnRoute::Import => {}
            os_actions::VpnRoute::Profile | os_actions::VpnRoute::DnsRouting => {
                let Some(profile) = profile else { return };
                if let Some(id) = self
                    .vault
                    .connections
                    .iter()
                    .find(|connection| connection.name == profile)
                    .map(|connection| connection.id)
                {
                    self.select(id);
                    self.detail_tab = DetailTab::Config;
                } else {
                    self.log(format!("VPN profile '{profile}' is no longer available"));
                }
            }
        }
    }

    /// Lay this device's live state over the shared fleet.
    fn refresh(&mut self) {
        self.vault.connections.clone_from(&self.shared.connections);
        let capture = self.mode == RunMode::Capture;
        for connection in &mut self.vault.connections {
            connection.status = self
                .live_status
                .get(&connection.id)
                .copied()
                .unwrap_or(ConnStatus::Disconnected);
            // A capture has no handshake to read; it shows a fixed one.
            if capture && connection.status == ConnStatus::Connected {
                connection.last_used = "Active demo".into();
                connection.last_handshake_secs = Some(34);
            }
        }
        self.vault.connections.sort_by(|a, b| {
            b.is_connected()
                .cmp(&a.is_connected())
                .then_with(|| a.name.cmp(&b.name))
        });
        self.local_selected = self
            .local_selected
            .filter(|id| self.vault.get(*id).is_some())
            .or_else(|| self.vault.connections.first().map(|c| c.id));
        self.vault.selected = self.local_selected;
    }

    /// Fold in the last probe and start the next one. The stored toggle never
    /// overrides reality: a tunnel that dropped out-of-band flips back to
    /// Disconnected, and one brought up elsewhere shows Connected. A tunnel
    /// whose transition is running is left to it. Only a live window that
    /// may execute probes.
    fn probe_device(&mut self) {
        if !self.mode.is_live() || !config::exec_allowed() {
            return;
        }
        if let Some(reading) = self.prober.take() {
            self.apply_reading(reading);
        }
        let connected = self
            .vault
            .connections
            .iter()
            .filter(|c| c.is_connected())
            .cloned()
            .collect();
        self.prober.start_if_due(connected, self.waker.clone());
    }

    fn apply_reading(&mut self, reading: probe::Reading) {
        if let Some(stored) = self.device.as_ref().map(Setting::get) {
            // Another window of this device may have set a profile up.
            self.local_details = stored.profiles;
            self.tor_global = stored.tor_global;
        }
        if let Some(profiles) = reading.profiles {
            for connection in &self.shared.connections {
                if self.transitions.contains(&connection.id) {
                    continue;
                }
                let local = self.local_details.get(&connection.id).and_then(|details| {
                    profiles
                        .iter()
                        .find(|profile| profile.uuid == details.network_manager_uuid)
                });
                if local.is_some() {
                    self.local_credentials.insert(connection.id);
                } else {
                    // Keep the local association as a recovery hint, but never
                    // advertise machine actions after its exact NM UUID vanished.
                    self.local_credentials.remove(&connection.id);
                }
                let status = if local.is_some_and(|profile| profile.active) {
                    ConnStatus::Connected
                } else {
                    ConnStatus::Disconnected
                };
                self.live_status.insert(connection.id, status);
            }
        }
        let connected: HashSet<ConnId> = self
            .live_status
            .iter()
            .filter(|(_, status)| **status == ConnStatus::Connected)
            .map(|(id, _)| *id)
            .collect();
        // Forget history for tunnels that are no longer up.
        self.throughput.retain(|id, _| connected.contains(id));
        for (id, transfer) in reading.transfers {
            if connected.contains(&id) {
                self.throughput.entry(id).or_default().push(transfer);
            }
        }
        self.refresh();
    }

    // ---- view state --------------------------------------------------

    fn select(&mut self, id: ConnId) {
        self.local_selected = Some(id);
        self.vault.selected = Some(id);
        self.detail_offset.set((0.0, 0.0));
        self.shell.show(NavPane::Content);
    }

    fn select_offset(&mut self, delta: i32) {
        if self.vault.connections.is_empty() {
            return;
        }
        let cur = self
            .vault
            .selected
            .and_then(|id| self.vault.index_of(id))
            .unwrap_or(0) as i32;
        let n = self.vault.connections.len() as i32;
        let next = (cur + delta).rem_euclid(n) as usize;
        // `next` is always `< n == self.vault.connections.len()` by
        // construction (rem_euclid against a positive `n`, guaranteed by the
        // `is_empty()` check above), so this is unreachable in practice —
        // `.get()` documents that instead of indexing directly.
        let Some(target) = self.vault.connections.get(next) else {
            return;
        };
        let id = target.id;
        self.select(id);
    }

    /// The sample profile named `name`, selected without opening anything.
    fn stage_selection(&mut self, name: &str) -> Option<ConnId> {
        let id = self
            .vault
            .connections
            .iter()
            .find(|connection| connection.name == name)?
            .id;
        self.local_selected = Some(id);
        self.vault.selected = Some(id);
        Some(id)
    }

    /// Stage the view state of a screenshot scenario; its profiles and this
    /// device's facts are already in place.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "confirm" => {
                if let Some(id) = self.vault.connections.first().map(|c| c.id) {
                    self.request_toggle(id);
                }
            }
            "add" | "import" => self.open_add(ConnKind::WireGuard),
            "detail" => self.shell.show(NavPane::Content),
            "config" => {
                self.shell.show(NavPane::Content);
                self.detail_tab = DetailTab::Config;
            }
            "logs" => {
                self.shell.show(NavPane::Content);
                self.detail_tab = DetailTab::Activity;
                self.activity
                    .push("NetworkManager: profile activation verified".into());
                self.activity
                    .push("No credentials were written to the shared document".into());
            }
            "loading" => self.shot_loading = true,
            "disconnected" | "connecting" => {
                self.stage_selection("Work");
            }
            "error" => {
                self.operation_error =
                    Some("The profile is incomplete: WireGuard Peer.PublicKey is missing.".into());
                self.retry_import_kind = Some(ConnKind::WireGuard);
            }
            "unprovisioned" => {
                if self.stage_selection("Home").is_some() {
                    self.shell.show(NavPane::Content);
                }
            }
            "setup" => {
                if let Some(id) = self.stage_selection("Home") {
                    self.provision_target = Some(id);
                    self.open_add(ConnKind::WireGuard);
                }
            }
            _ => {}
        }
    }

    /// Begin connecting/disconnecting a connection → raises a confirm that
    /// shows the exact command.
    fn request_toggle(&mut self, id: ConnId) {
        let Some(c) = self.vault.get(id) else { return };
        if !self.local_credentials.contains(&id) {
            self.operation_error = Some(format!(
                "Credentials are unavailable on this device for '{}'. Import the exact provider profile here before connecting.",
                c.name
            ));
            self.retry_import_kind = Some(c.kind);
            return;
        }
        if c.status == ConnStatus::Connecting || self.transitions.contains(&id) {
            self.log(format!("{} is already changing state", c.name));
            return;
        }
        self.operation_error = None;
        self.retry_import_kind = None;
        let Some(details) = self.local_details.get(&id) else {
            self.operation_error = Some("Credentials are unavailable on this device. Import the exact provider profile to provision it securely.".into());
            self.retry_import_kind = Some(c.kind);
            return;
        };
        self.pending = Some(if c.is_connected() {
            Pending::Disconnect(
                id,
                config::disconnect_local_plan(c, &details.network_manager_uuid),
            )
        } else {
            Pending::Connect(
                id,
                config::connect_local_plan(c, &details.network_manager_uuid),
            )
        });
        self.modal = Modal::Confirm;
    }

    fn request_tor_global(&mut self) {
        self.operation_error = None;
        self.retry_import_kind = None;
        let next = !self.tor_global;
        self.pending = Some(Pending::TorGlobal(next, config::tor_plan(next)));
        self.modal = Modal::Confirm;
    }

    /// Run the pending action after the user confirmed the exact command:
    /// the effect every caller reaches, a job that changes nothing it has
    /// not seen succeed — the UI never claims a tunnel is connected on the
    /// strength of an optimistic guess.
    fn confirm_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let effect = match p {
            Pending::Connect(id, _) => VpnEffect::Connect {
                connection: id.to_string(),
            },
            Pending::Disconnect(id, _) => VpnEffect::Disconnect {
                connection: id.to_string(),
            },
            Pending::TorGlobal(..) => VpnEffect::ToggleTorGlobal,
        };
        self.invoke(&effect);
        self.modal = Modal::None;
    }

    /// Invoke one of the app's effects through the door every caller uses.
    fn invoke(&self, effect: &VpnEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    fn log(&mut self, line: String) {
        self.activity.push(line);
        if self.activity.len() > 50 {
            let drop = self.activity.len() - 50;
            self.activity.drain(0..drop);
        }
    }

    /// Import a new profile of `kind`, through the same effect the palette
    /// offers (and its confirmation).
    fn queue_import(&mut self, kind: ConnKind) {
        self.provision_target = None;
        self.invoke(&match kind {
            ConnKind::WireGuard => VpnEffect::ImportWireguard,
            ConnKind::OpenVpn => VpnEffect::ImportOpenvpn,
        });
    }

    /// Set the shared profile `id` up on this device.
    fn request_local_provision(&mut self, id: ConnId) {
        if self.vault.get(id).is_none() {
            return;
        }
        self.provision_target = Some(id);
        self.invoke(&VpnEffect::ProvisionLocal {
            connection: id.to_string(),
        });
    }

    pub(crate) fn toggle_conn_tor(&self, id: ConnId) {
        let next = self.vault.get(id).is_none_or(|c| !c.tor);
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(Vpn::paths().connections.item(&Vpn::item(id)).tor.set(next));
        }
    }

    fn open_add(&mut self, kind: ConnKind) {
        self.operation_error = None;
        self.retry_import_kind = None;
        self.add_kind = kind;
        self.modal = Modal::Add;
    }

    /// Clear a transient failure after it has been announced. Kept explicit so
    /// mouse, keyboard, and semantic actions share one recovery path.
    fn dismiss_error(&mut self) {
        self.operation_error = None;
        self.retry_import_kind = None;
        self.provision_target = None;
    }

    /// Import the exact protocol that failed again. This only runs when the
    /// failure was produced by the portal/import pipeline, never for a tunnel
    /// transition where replaying a privileged command blindly would be unsafe.
    fn retry_import(&mut self) {
        let Some(kind) = self.retry_import_kind else {
            return;
        };
        match self.provision_target {
            Some(id) => self.request_local_provision(id),
            None => self.queue_import(kind),
        }
    }

    pub(crate) fn commit_add(&mut self) {
        match self.provision_target {
            Some(id) => self.request_local_provision(id),
            None => self.queue_import(self.add_kind),
        }
        self.modal = Modal::None;
        self.log(format!(
            "waiting for approval to import a complete {} provider profile",
            self.add_kind.long_name()
        ));
    }

    /// Store this device's profiles in its device-local setting.
    fn save_device(&self) -> Result<(), String> {
        let Some(device) = &self.device else {
            return Ok(());
        };
        device
            .update(|stored| stored.profiles.clone_from(&self.local_details))
            .map_err(|error| error.to_string())
    }
}

// ---- effects -------------------------------------------------------------

impl VpnApp {
    /// Answer `effect`, from a control, the palette, an agent or a script.
    pub(crate) fn answer(
        &mut self,
        effect: VpnEffect,
        cx: &mut EffectCx<Vpn, Self>,
    ) -> EffectAnswer {
        match effect {
            VpnEffect::ImportWireguard => self.start_import(ConnKind::WireGuard, None, cx),
            VpnEffect::ImportOpenvpn => self.start_import(ConnKind::OpenVpn, None, cx),
            VpnEffect::ProvisionLocal { connection } => {
                let id = effects::connection_id(&connection)?;
                let existing = self
                    .vault
                    .get(id)
                    .cloned()
                    .ok_or_else(|| "VPN profile is no longer saved".to_string())?;
                self.start_import(existing.kind, Some(existing), cx)
            }
            VpnEffect::Connect { connection } => {
                self.start_transition(effects::connection_id(&connection)?, true, cx)
            }
            VpnEffect::Disconnect { connection } => {
                self.start_transition(effects::connection_id(&connection)?, false, cx)
            }
            VpnEffect::DeleteConnection { connection } => {
                self.start_delete(effects::connection_id(&connection)?, cx)
            }
            VpnEffect::ToggleTorGlobal => self.start_tor(cx),
        }
    }

    /// The effects that differ from their declaration now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if self.tor_global {
            statuses.push(EffectStatus::labelled(
                VpnEffect::TOGGLE_TOR_GLOBAL,
                "Stop routing all traffic over Tor",
            ));
        }
        if self.tor_changing {
            statuses.push(EffectStatus::unavailable(
                VpnEffect::TOGGLE_TOR_GLOBAL,
                "transition_in_progress",
                "The Tor service is already changing state",
            ));
        }
        if self.importing {
            let reason = "Another VPN profile import is in progress";
            statuses.push(EffectStatus::unavailable(
                VpnEffect::IMPORT_WIREGUARD,
                "import_in_progress",
                reason,
            ));
            statuses.push(EffectStatus::unavailable(
                VpnEffect::IMPORT_OPENVPN,
                "import_in_progress",
                reason,
            ));
            statuses.push(EffectStatus::unavailable(
                VpnEffect::PROVISION_LOCAL,
                "import_in_progress",
                reason,
            ));
        }
        statuses
    }

    /// Each saved profile's effects, with what this device can do now.
    pub(crate) fn profile_rows(&self) -> Vec<LiveRow> {
        self.vault
            .connections
            .iter()
            .map(|connection| {
                effects::profile_row(&effects::ProfileHere {
                    connection,
                    provisioned: self.is_provisioned(connection.id),
                    changing: self.transitions.contains(&connection.id),
                })
            })
            .collect()
    }

    fn is_provisioned(&self, id: ConnId) -> bool {
        self.local_credentials.contains(&id)
            && self
                .local_details
                .get(&id)
                .is_some_and(|details| !details.network_manager_uuid.is_empty())
    }

    fn start_import(
        &mut self,
        kind: ConnKind,
        target: Option<Connection>,
        cx: &mut EffectCx<Vpn, Self>,
    ) -> EffectAnswer {
        if self.importing {
            return Err("another VPN profile import is already in progress".into());
        }
        self.operation_error = None;
        self.retry_import_kind = None;
        self.import_hint = kind;
        self.provision_target = target.as_ref().map(|connection| connection.id);
        let picked = import::request(kind).send();
        self.importing = true;
        self.log(format!("choose a .{} file to import…", kind.ext()));
        let job = Import {
            hint: kind,
            target,
            picked,
        };
        Ok(cx.thread(
            move |progress| import::run(job, &progress),
            |app: &mut Self, imported, cx| app.finish_import(imported, cx),
        ))
    }

    fn finish_import(&mut self, imported: Imported, cx: &EffectCx<Vpn, Self>) -> EffectAnswer {
        self.importing = false;
        let target = self.provision_target.take();
        match imported {
            Imported::Cancelled => Err("Import cancelled".into()),
            Imported::Failed(message) => {
                self.operation_error = Some(message.clone());
                self.retry_import_kind = Some(self.import_hint);
                self.provision_target = target;
                self.log(message.clone());
                Err(message)
            }
            // A post-import ambiguity is not retryable: retrying could
            // create a duplicate credential-bearing `NetworkManager` profile.
            Imported::Ambiguous(message) => {
                self.operation_error = Some(message.clone());
                self.retry_import_kind = None;
                self.log(message.clone());
                Err(message)
            }
            Imported::Done(done) => {
                let import::Done {
                    connection,
                    details,
                    log,
                } = *done;
                self.log(log);
                let id = connection.id;
                // `NetworkManager` holds the profile now; the shared fleet
                // learns its secret-free identity, as the palette would add it.
                if target.is_none() {
                    let added = Vpn::paths().connections.insert_as(
                        &Vpn::item(id),
                        usize::MAX,
                        &Profile::of(&connection),
                    );
                    if let Ok(added) = Invoke::edit(&added) {
                        cx.submit(added);
                    }
                }
                let saved = if details.network_manager_uuid.is_empty() {
                    Ok(())
                } else {
                    self.local_credentials.insert(id);
                    self.local_details.insert(id, details);
                    self.save_device()
                };
                self.select(id);
                match saved {
                    Ok(()) => {
                        self.operation_error = None;
                        self.retry_import_kind = None;
                        Ok(Effected::done())
                    }
                    Err(error) => {
                        let message = format!(
                            "Profile imported but its device-local association could not be saved: {error}. The external profile exists; do not retry import, reopen NetworkManager to recover it."
                        );
                        self.operation_error = Some(message.clone());
                        self.retry_import_kind = None;
                        Err(message)
                    }
                }
            }
        }
    }

    fn start_transition(
        &mut self,
        id: ConnId,
        up: bool,
        cx: &mut EffectCx<Vpn, Self>,
    ) -> EffectAnswer {
        let connection = self
            .vault
            .get(id)
            .cloned()
            .ok_or_else(|| "VPN connection is no longer saved".to_string())?;
        if connection.status == ConnStatus::Connecting || self.transitions.contains(&id) {
            return Err("The tunnel is already changing state".into());
        }
        if up && connection.is_connected() {
            return Err("This tunnel is already up on this device".into());
        }
        if !up && !connection.is_connected() {
            return Err("This tunnel is not up on this device".into());
        }
        let uuid = self
            .local_details
            .get(&id)
            .filter(|_| self.local_credentials.contains(&id))
            .map(|details| details.network_manager_uuid.clone())
            .ok_or_else(|| {
                format!(
                    "Credentials are unavailable on this device for '{}'. Import the exact provider profile here before connecting.",
                    connection.name
                )
            })?;
        let plan = if up {
            config::connect_local_plan(&connection, &uuid)
        } else {
            config::disconnect_local_plan(&connection, &uuid)
        };
        self.operation_error = None;
        self.transitions.insert(id);
        if up {
            self.live_status.insert(id, ConnStatus::Connecting);
            self.refresh();
        }
        Ok(cx.thread(
            move |progress| {
                progress.report(plan.display(), None, None);
                let ran = config::execute(&plan);
                let ready = (up && ran.is_ok()).then(|| config::network_manager_active(&uuid));
                (plan, ran, ready)
            },
            move |app: &mut Self, (plan, ran, ready), _| {
                app.finish_transition(id, up, &plan, ran, ready)
            },
        ))
    }

    fn finish_transition(
        &mut self,
        id: ConnId,
        up: bool,
        plan: &CommandPlan,
        ran: Result<String, String>,
        ready: Option<Result<bool, String>>,
    ) -> EffectAnswer {
        self.transitions.remove(&id);
        let answer = match (up, ran) {
            (true, Ok(out)) => {
                if matches!(ready, Some(Ok(true))) {
                    self.live_status.insert(id, ConnStatus::Connected);
                    self.log(format!("$ {}  → up {}", plan.display(), out));
                    Ok(Effected::done())
                } else {
                    // Stay disconnected — readiness was not seen.
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    let reason = match ready {
                        Some(Ok(false)) => {
                            "NetworkManager still reports the profile inactive".into()
                        }
                        Some(Err(error)) => error,
                        Some(Ok(true)) | None => "the readiness state changed unexpectedly".into(),
                    };
                    let message = format!(
                        "NetworkManager accepted the request, but readiness could not be confirmed: {reason}"
                    );
                    self.operation_error = Some(message.clone());
                    self.log(format!("$ {}  → not ready: {reason}", plan.display()));
                    Err(message)
                }
            }
            (true, Err(error)) => {
                // Stay disconnected — we did NOT connect.
                self.live_status.insert(id, ConnStatus::Disconnected);
                self.operation_error = Some(format!("Could not connect: {error}"));
                self.log(format!("$ {}  → not connected: {}", plan.display(), error));
                Err(error)
            }
            (false, Ok(out)) => {
                self.live_status.insert(id, ConnStatus::Disconnected);
                self.log(format!("$ {}  → ok {}", plan.display(), out));
                Ok(Effected::done())
            }
            (false, Err(error)) => {
                self.operation_error = Some(format!("Could not disconnect: {error}"));
                self.log(format!("$ {}  → failed: {}", plan.display(), error));
                Err(error)
            }
        };
        self.refresh();
        answer
    }

    fn start_delete(&mut self, id: ConnId, cx: &mut EffectCx<Vpn, Self>) -> EffectAnswer {
        if self.deleting.is_some() {
            return Err("another VPN profile deletion is already running".into());
        }
        if self.vault.get(id).is_none() {
            return Err("VPN profile is no longer saved".into());
        }
        let uuid = self
            .local_details
            .get(&id)
            .map(|details| details.network_manager_uuid.clone())
            .filter(|uuid| !uuid.is_empty())
            .ok_or_else(|| {
                "Credentials are unavailable on this device; local NetworkManager profile cannot be deleted here".to_string()
            })?;
        self.deleting = Some(id);
        Ok(cx.thread(
            move |_| {
                if config::exec_allowed() {
                    os_actions::delete_vpn_connection_uuid(&uuid)
                } else {
                    Err("execution disabled in test process".into())
                }
            },
            move |app: &mut Self, deleted, cx| app.finish_delete(id, deleted, cx),
        ))
    }

    fn finish_delete(
        &mut self,
        id: ConnId,
        deleted: Result<(), String>,
        cx: &EffectCx<Vpn, Self>,
    ) -> EffectAnswer {
        self.deleting = None;
        if let Err(message) = deleted {
            self.operation_error =
                Some(format!("Could not delete the local VPN profile: {message}"));
            return Err(message);
        }
        let removed = self.local_details.remove(&id);
        let had_credentials = self.local_credentials.remove(&id);
        if let Err(error) = self.save_device() {
            if let Some(details) = removed {
                self.local_details.insert(id, details);
            }
            if had_credentials {
                self.local_credentials.insert(id);
            }
            self.operation_error = Some(format!(
                "NetworkManager deleted the profile, but local association cleanup failed: {error}. The shared profile was retained for recovery."
            ));
            return Err("NetworkManager deleted the profile but local cleanup failed".into());
        }
        // The local profile is gone; the shared fleet forgets it too.
        if let Ok(removed) = Invoke::edit(&Vpn::paths().connections.remove(&Vpn::item(id))) {
            cx.submit(removed);
        }
        self.local_selected = None;
        self.live_status.remove(&id);
        self.operation_error = None;
        Ok(Effected::done())
    }

    /// Start or stop this device's Tor service. Whether traffic goes over
    /// Tor is this device's state, so it is remembered here, never shared:
    /// a peer that saw a shared toggle would claim a routing it never set.
    fn start_tor(&mut self, cx: &mut EffectCx<Vpn, Self>) -> EffectAnswer {
        if self.tor_changing {
            return Err("The Tor service is already changing state".into());
        }
        let next = !self.tor_global;
        let plan = config::tor_plan(next);
        self.tor_changing = true;
        Ok(cx.thread(
            move |_| {
                let ran = config::execute(&plan);
                (plan, ran)
            },
            move |app: &mut Self, (plan, ran), _| {
                app.tor_changing = false;
                match ran {
                    Ok(out) => {
                        app.tor_global = next;
                        app.log(format!("$ {}  → ok {}", plan.display(), out));
                        if let Some(device) = &app.device {
                            device
                                .update(|stored| stored.tor_global = next)
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(Effected::done())
                    }
                    Err(error) => {
                        app.log(format!("$ {}  → failed: {}", plan.display(), error));
                        Err(error)
                    }
                }
            },
        ))
    }
}

impl VpnApp {
    fn new_shell(intents: &Intents, main_action: &ToolbarActionSlot) -> NavShell {
        NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .overview_header("Connections", vec![Box::new(view::import_button(intents))])
            .content_header(
                "",
                vec![Box::new(view::MainActionButton::new(main_action, intents))],
            )
            .with_id("vpn-shell")
    }
    fn rates_for(&self, connection: &Connection) -> Rates {
        if !connection.is_connected() {
            return Rates::default();
        }
        if self.mode == RunMode::Capture {
            return demo_rates();
        }
        let history = self.throughput.get(&connection.id);
        let (down, up) = history
            .map(TransferHistory::rate_series)
            .unwrap_or_default();
        Rates {
            down,
            up,
            latest: history.and_then(TransferHistory::latest),
        }
    }
    fn handle_key_press(&mut self, logical_key: &Key) -> bool {
        match logical_key {
            Key::Named(NamedKey::Escape) if self.operation_error.is_some() => self.dismiss_error(),
            Key::Named(NamedKey::ArrowDown) => self.select_offset(1),
            Key::Named(NamedKey::ArrowUp) => self.select_offset(-1),
            Key::Character(c) => match c.as_str() {
                "1" => self.detail_tab = DetailTab::Status,
                "2" => self.detail_tab = DetailTab::Config,
                "3" => self.detail_tab = DetailTab::Activity,
                _ => return false,
            },
            _ => return false,
        }
        self.dirty = true;
        true
    }
}
fn demo_rates() -> Rates {
    const MIB: f64 = 1024.0 * 1024.0;
    let wave = |i: usize, base: f64, swing: f64, burst: f64| {
        let t = i as f64;
        let burst = if (34..44).contains(&i) { burst } else { 0.0 };
        (base + swing * (t * 0.37).sin() + 0.4 * swing * (t * 1.13).cos() + burst) * MIB
    };
    Rates {
        down: (0..THROUGHPUT_SAMPLES)
            .map(|i| wave(i, 3.6, 0.9, 2.8))
            .collect(),
        up: (0..THROUGHPUT_SAMPLES)
            .map(|i| wave(i, 0.55, 0.2, 0.3))
            .collect(),
        latest: Some(Transfer {
            rx: 1_976_000_000,
            tx: 223_000_000,
        }),
    }
}
fn synth_logs(c: &Connection, activity: &[String]) -> Vec<String> {
    let mut out: Vec<String> = activity.iter().rev().take(8).cloned().collect();
    if out.is_empty() {
        out.push(if c.is_connected() {
            format!("{} tunnel is up on this device", c.kind.long_name())
        } else {
            "The tunnel is down on this device".into()
        });
    }
    out
}
impl VpnApp {
    /// The problem or wait the person should see, if any.
    fn notice_state(&self) -> Option<(NoticeKind, String, bool)> {
        if let Some(error) = &self.operation_error {
            return Some((
                NoticeKind::Error,
                error.clone(),
                self.retry_import_kind.is_some(),
            ));
        }
        (self.importing || self.shot_loading)
            .then(|| (NoticeKind::Waiting(self.import_hint), String::new(), false))
    }

    /// Rebuild the overview and the content when what they show changed.
    fn refresh_view(&mut self) {
        let notice_state = self.notice_state();
        let notice = notice_state.as_ref().map(|(kind, message, retry)| Notice {
            kind: *kind,
            message,
            retry: *retry,
        });
        if self.vault.connections.is_empty() {
            let key = format!("empty:{notice_state:?}:{}", self.view_rev);
            if self.detail_key.as_deref() == Some(key.as_str()) {
                return;
            }
            self.detail_key = Some(key);
            self.list_key = None;
            self.main_action.set(None);
            self.shell.set_overview(None);
            self.shell.set_content_title("");
            let content = match &notice {
                Some(notice) => view::notice_page(notice, &self.intents),
                None => view::empty_state(&self.intents),
            };
            self.shell.set_content(content);
            self.dirty = true;
            return;
        }

        let list_key = ListKey {
            rows: self
                .vault
                .connections
                .iter()
                .map(|c| {
                    (
                        c.clone(),
                        self.local_details.get(&c.id).map(|d| d.endpoint.clone()),
                        self.is_provisioned(c.id),
                    )
                })
                .collect(),
            selected: self.vault.selected,
            tor_global: self.tor_global,
            rev: self.view_rev,
        };
        if self.list_key.as_ref() != Some(&list_key) {
            let model = view::OverviewModel {
                entries: self
                    .vault
                    .connections
                    .iter()
                    .map(|connection| view::ListEntry {
                        connection,
                        details: self.local_details.get(&connection.id),
                        provisioned: self.is_provisioned(connection.id),
                    })
                    .collect(),
                selected: self.vault.selected,
                tor_global: self.tor_global,
            };
            let overview = view::overview(&model, &self.intents, &self.list_offset);
            self.shell.set_overview(Some(overview));
            self.list_key = Some(list_key);
            self.dirty = true;
        }

        let Some(connection) = self.vault.selected_conn() else {
            self.main_action.set(None);
            return;
        };
        let details = self.local_details.get(&connection.id);
        let provisioned = self.is_provisioned(connection.id);
        let rates = self.rates_for(connection);
        let activity = synth_logs(connection, &self.activity);
        let tor_global = self.tor_global;
        let key = format!(
            "{connection:?}{details:?}{provisioned}{:?}{rates:?}{activity:?}{notice_state:?}{tor_global}{}",
            self.detail_tab, self.view_rev
        );
        if self.detail_key.as_deref() == Some(key.as_str()) {
            return;
        }
        let model = view::DetailModel {
            connection,
            details,
            tab: self.detail_tab,
            rates: &rates,
            activity,
            notice,
            tor_global,
        };
        let content = view::detail(&model, &self.intents, &self.detail_offset);
        self.main_action.set(Some(ToolbarAction {
            id: connection.id,
            action: MainAction::of(connection, provisioned),
        }));
        let title = connection.name.clone();
        self.detail_key = Some(key);
        self.shell.set_content_title(title);
        self.shell.set_content(content);
        self.dirty = true;
    }

    /// Open, rebuild or close the sheet to match [`Self::modal`].
    fn refresh_sheet(&mut self) {
        if self.modal == Modal::Confirm && self.pending.is_none() {
            self.modal = Modal::None;
        }
        let key = match self.modal {
            Modal::None => None,
            Modal::Add => Some(format!(
                "add:{:?}:{:?}",
                self.add_kind, self.provision_target
            )),
            Modal::Confirm => self.pending.as_ref().map(|p| format!("confirm:{p:?}")),
        };
        if key == self.dialog_key {
            return;
        }
        self.dialog = key.as_ref().and_then(|_| self.build_dialog());
        self.dialog_key = key;
        self.dialog_changed = true;
        self.dirty = true;
    }

    fn build_dialog(&self) -> Option<ModalDialog<Column>> {
        let intents = &self.intents;
        match self.modal {
            Modal::None => None,
            Modal::Add => {
                let target = self.provision_target.and_then(|id| self.vault.get(id));
                let (title, kind, fixed) = match target {
                    Some(connection) => (
                        format!("Set up {} on this device", connection.name),
                        connection.kind,
                        true,
                    ),
                    None => ("Import VPN configuration".to_owned(), self.add_kind, false),
                };
                Some(
                    ModalDialog::new(title, view::import_body(kind, fixed, intents))
                        .open(true)
                        .with_test_id("vpn-import-sheet")
                        .primary_action("Choose file", intents.sender(Intent::ChooseFile))
                        .secondary_action("Cancel", intents.sender(Intent::CloseImport))
                        .on_dismiss(intents.sender(Intent::CloseImport)),
                )
            }
            Modal::Confirm => {
                let (title, primary, summary, command) = self.confirm_copy()?;
                let dialog = ModalDialog::new(title, view::confirm_body(&summary, &command))
                    .open(true)
                    .with_test_id("vpn-confirm-sheet")
                    .secondary_action("Cancel", intents.sender(Intent::CancelConfirm))
                    .on_dismiss(intents.sender(Intent::CancelConfirm));
                Some(if matches!(self.pending, Some(Pending::Disconnect(..))) {
                    dialog
                        .primary_action(primary, intents.sender(Intent::Confirm))
                        .with_destructive(true)
                } else {
                    dialog.primary_action(primary, intents.sender(Intent::Confirm))
                })
            }
        }
    }

    /// The pending action's sheet: what is decided, the button that does it,
    /// what happens, and the exact command that runs.
    fn confirm_copy(&self) -> Option<(String, &'static str, String, String)> {
        let name = |id: ConnId| {
            self.vault
                .get(id)
                .map_or_else(|| "the tunnel".to_owned(), |c| c.name.clone())
        };
        Some(match self.pending.as_ref()? {
            Pending::Connect(id, plan) => (
                format!("Connect to {}", name(*id)),
                "Connect",
                format!(
                    "NetworkManager brings the {} tunnel up on this device.",
                    name(*id)
                ),
                plan.display(),
            ),
            Pending::Disconnect(id, plan) => (
                format!("Disconnect from {}", name(*id)),
                "Disconnect",
                format!(
                    "NetworkManager takes the {} tunnel down on this device.",
                    name(*id)
                ),
                plan.display(),
            ),
            Pending::TorGlobal(true, plan) => (
                "Route all traffic over Tor".to_owned(),
                "Turn on",
                "The Tor service starts on this device and VPN traffic is routed through it."
                    .to_owned(),
                plan.display(),
            ),
            Pending::TorGlobal(false, plan) => (
                "Stop routing over Tor".to_owned(),
                "Turn off",
                "The Tor service stops on this device; VPN traffic no longer goes through it."
                    .to_owned(),
                plan.display(),
            ),
        })
    }

    /// Connect, disconnect, or set up on this device: the connection's main
    /// action as it stands.
    fn main_action(&mut self, id: ConnId) {
        let Some(connection) = self.vault.get(id) else {
            return;
        };
        match MainAction::of(connection, self.is_provisioned(id)) {
            MainAction::Connecting => {}
            MainAction::SetUp => {
                let kind = connection.kind;
                self.provision_target = Some(id);
                self.open_add(kind);
            }
            _ => self.request_toggle(id),
        }
    }

    /// Apply what the window's controls asked for.
    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            match intent {
                Intent::Select(id) => self.select(id),
                Intent::SetTab(tab) => {
                    self.detail_tab = tab;
                    self.detail_offset.set((0.0, 0.0));
                }
                Intent::MainAction(id) => self.main_action(id),
                Intent::ToggleTor(id) => self.toggle_conn_tor(id),
                Intent::ToggleTorGlobal => self.request_tor_global(),
                Intent::OpenImport => {
                    self.provision_target = None;
                    self.open_add(ConnKind::WireGuard);
                }
                Intent::SetImportKind(kind) => self.add_kind = kind,
                Intent::ChooseFile => self.commit_add(),
                Intent::CloseImport => {
                    self.modal = Modal::None;
                    self.provision_target = None;
                }
                Intent::Confirm => {
                    self.confirm_pending();
                    self.view_rev += 1;
                }
                Intent::CancelConfirm => {
                    self.pending = None;
                    self.modal = Modal::None;
                    // A switch the person flipped returns to the saved value.
                    self.view_rev += 1;
                }
                Intent::RetryImport => self.retry_import(),
                Intent::DismissError => self.dismiss_error(),
            }
            self.dirty = true;
        }
    }
}

impl ui::widget::Decorator for VpnApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn test_id(&self) -> Option<&str> {
        Some("vpn_app")
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.mode.is_live() {
            self.probe_device();
            self.apply_pending_tool_intent();
        }
        self.drain_intents();
        self.refresh_view();
        self.refresh_sheet();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.drain_intents();
        self.refresh_view();
        self.refresh_sheet();
        self.shell.update(state, path) || self.dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let ground = theme.tokens.colors.surfaces.content;
        if self.painted_ground.is_some_and(|painted| painted != ground) {
            self.view_rev += 1;
            self.refresh_view();
        }
        self.painted_ground = Some(ground);
        let node = self.shell.build_layout(taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The open sheet is modal: it takes every event, and nothing reaches
        // the window behind its scrim.
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
            );
            *ctx.handled = true;
            // The sheet's hover and press faces repaint with the window.
            self.dirty = true;
            self.drain_intents();
            return;
        }
        self.shell.handle_event(event, ctx);
        self.drain_intents();
        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && !ctx.modifiers.state().control_key()
        {
            *ctx.handled = self.handle_key_press(logical_key);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.shell.children();
        if let Some(dialog) = &self.dialog {
            children.push(dialog);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.shell.for_each_child(visitor);
        if let Some(dialog) = &self.dialog {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self.shell.children_mut();
        if let Some(dialog) = self.dialog.as_mut() {
            children.push(dialog);
        }
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dialog = std::mem::take(&mut self.dialog_changed);
        self.shell.take_structure_dirty() || dialog
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames (and thus `sync`) coming while a portal import is in
        // flight, and while a real tunnel is up so its live kernel counters
        // keep refreshing. The rates shown are measured, never fabricated —
        // with execution disabled nothing polls at all.
        self.importing
            || (config::exec_allowed()
                && self.mode.is_live()
                && self.vault.connections.iter().any(Connection::is_connected))
            || self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.dialog.as_ref().is_some_and(Widget::needs_redraw)
            || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }

    fn modal_capture(&self) -> Option<Rect> {
        match &self.dialog {
            Some(dialog) => Some(dialog.dialog_rect()),
            None => self.shell.modal_capture(),
        }
    }

    fn traps_focus(&self) -> bool {
        self.dialog.is_some() || self.shell.traps_focus()
    }
}

impl Default for VpnApp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_vault;
    use crate::resource::VpnShell;
    use automerge::AutoCommit;
    use ui::framework::{ResourceApp, testing::TestApp};
    use ui::headless::Framed;
    use ui::portal::PortalOutcome;
    use ui::tilo_resource::Invoke;
    use vello::kurbo::Point;
    fn sample() -> VpnApp {
        let mut app = VpnApp::new();
        app.mode = RunMode::Capture;
        app.apply_facts(&DeviceFacts::demo(&sample_vault()));
        app.present_vault(&Rc::new(sample_vault()));
        app
    }
    fn named(app: &VpnApp, name: &str) -> ConnId {
        app.vault
            .connections
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    }
    fn live_with_sample() -> TestApp<VpnShell> {
        let mut app = TestApp::<VpnShell>::new().unwrap();
        for (at, connection) in sample_vault().connections.iter().enumerate() {
            let added = Vpn::paths().connections.insert_as(
                &Vpn::item(connection.id),
                at,
                &Profile::of(connection),
            );
            app.invoke(Invoke::edit(&added).unwrap()).unwrap();
        }
        app.settle();
        app
    }
    fn published_row(app: &mut TestApp<VpnShell>, id: ConnId) -> ui::tilo_actions::Resource {
        fn find(
            resources: &[ui::tilo_actions::Resource],
            id: &str,
        ) -> Option<ui::tilo_actions::Resource> {
            resources.iter().find_map(|resource| {
                if resource.id.as_str() == id {
                    Some(resource.clone())
                } else {
                    find(&resource.children, id)
                }
            })
        }
        find(
            &app.harness().resources(),
            &effects::connection_resource_id(id),
        )
        .expect("the profile is published")
    }
    fn provider_file(stem: &str, secret: &str, endpoint: &str) -> std::path::PathBuf {
        let raw = format!(
            "[Interface]\nPrivateKey = {secret}\nAddress = 10.9.0.2/32\nDNS = 9.9.9.9\n\n[Peer]\nPublicKey = abc=\nEndpoint = {endpoint}\nAllowedIPs = 0.0.0.0/0\n"
        );
        let path = std::env::temp_dir().join(format!("{stem}-{}.conf", uuid::Uuid::new_v4()));
        std::fs::write(&path, raw).unwrap();
        path
    }
    #[test]
    fn the_live_app_starts_empty_and_selection_follows_the_fleet() {
        // Case `live_app_starts_empty_and_writes_nothing`.
        {
            let app = TestApp::<VpnShell>::new().unwrap();
            assert!(
                Vault::of(&app.state()).connections.is_empty(),
                "the live app fabricates no tunnels"
            );
        }
        // Case `typed_profile_context_selects_profile_and_config_tab_after_load`.
        {
            let mut app = sample();
            app.pending_tool_intent = Some(os_actions::ToolIntent::Vpn {
                route: os_actions::VpnRoute::Profile,
                profile: Some("Work".into()),
            });
            app.apply_pending_tool_intent();
            assert_eq!(app.vault.selected, Some(named(&app, "Work")));
            assert_eq!(app.detail_tab, DetailTab::Config);
            assert!(app.pending_tool_intent.is_none());
        }
        // Case `select_offset_wraps`.
        {
            let mut app = sample();
            let first = app.vault.selected.unwrap();
            app.select_offset(-1);
            let last = app.vault.selected.unwrap();
            assert_ne!(first, last);
            app.select_offset(1);
            assert_eq!(app.vault.selected.unwrap(), first);
        }
    }
    #[test]
    fn every_capability_is_published_with_this_devices_availability() {
        let mut app = live_with_sample();
        let surface = app.agent_surface();
        let actions_on = |resource: &str| -> Vec<String> {
            surface
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["resource"] == resource)
                .map(|row| row["action"].as_str().unwrap().to_owned())
                .collect()
        };
        let vault = actions_on("vpn.vault");
        for id in [
            "import-wireguard",
            "import-openvpn",
            "provision-local",
            "connect",
            "disconnect",
            "delete-connection",
            "toggle-tor-global",
            "field-edit",
        ] {
            assert!(vault.iter().any(|action| action == id), "{id}: {vault:?}");
        }
        let home = Vault::of(&app.state())
            .connections
            .iter()
            .find(|c| c.name == "Home")
            .unwrap()
            .id;
        let row = actions_on(&effects::connection_resource_id(home));
        for id in [
            "provision-local",
            "connect",
            "disconnect",
            "delete-connection",
        ] {
            assert!(row.iter().any(|action| action == id), "{id}: {row:?}");
        }

        // Nothing is set up on this device: its machine actions say so.
        let published = published_row(&mut app, home);
        let availability = |id: &str| {
            published
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap()
                .contract
                .availability
                .clone()
        };
        for id in ["connect", "delete-connection"] {
            assert!(
                matches!(availability(id), ui::tilo_actions::Availability::Unavailable { ref code, .. } if code == "local_credentials_required"),
                "{id}"
            );
        }
        assert!(matches!(
            availability("disconnect"),
            ui::tilo_actions::Availability::Unavailable { ref code, .. } if code == "not_connected"
        ));
        assert_eq!(
            availability("provision-local"),
            ui::tilo_actions::Availability::Available
        );
        let connect = published
            .actions
            .iter()
            .find(|action| action.id == "connect")
            .unwrap();
        assert_eq!(connect.contract.result, ui::tilo_actions::ResultSpec::Job);
        assert_eq!(connect.contract.kind, ui::tilo_actions::ActionKind::Outside);

        // A connect asked for anyway is refused, and nothing claims a tunnel.
        assert!(
            app.effect(&VpnEffect::Connect {
                connection: home.to_string(),
            })
            .is_err()
        );
        app.settle();
        let row = published_row(&mut app, home);
        assert!(row.label.starts_with("Home"), "{}", row.label);
    }
    #[test]
    fn a_profiles_tor_preference_and_name_are_shared_edits() {
        let mut app = live_with_sample();
        let relay = Vault::of(&app.state())
            .connections
            .iter()
            .find(|c| c.name == "Cloud Relay")
            .unwrap()
            .clone();
        let profile = Vpn::paths().connections.item(&Vpn::item(relay.id));
        for edit in [profile.tor.set(!relay.tor), profile.name.replace("Relay")] {
            app.call("field-edit", serde_json::json!({ "edit": edit }))
                .unwrap();
        }
        app.settle();
        let after = Vault::of(&app.state())
            .connections
            .into_iter()
            .find(|c| c.id == relay.id)
            .unwrap();
        assert_eq!(after.tor, !relay.tor);
        assert_eq!(after.name, "Relay");
    }
    #[test]
    fn importing_keeps_secrets_out_of_the_document() {
        // Case `a_new_import_adds_only_a_secret_free_identity`.
        {
            let mut app = TestApp::<VpnShell>::new().unwrap();
            let path = provider_file("home", "c2VjcmV0", "imported.example:51820");
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            let imported = ui::host::scoped(&services, || app.effect(&VpnEffect::ImportWireguard));
            assert!(imported.is_ok(), "{imported:?}");
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Import WireGuard tunnel"
            );
            app.settle();
            let state = Vault::of(&app.state());
            assert_eq!(state.connections.len(), 1);
            let profile = &state.connections[0];
            assert!(profile.name.starts_with("home-"), "{}", profile.name);
            assert_eq!(profile.kind, ConnKind::WireGuard);
            let saved = app.read_document(|doc| doc.clone().save());
            let saved = String::from_utf8_lossy(&saved);
            assert!(
                !saved.contains("c2VjcmV0"),
                "no private key in the document"
            );
            assert!(!saved.contains("imported.example"), "no endpoint either");
            let _ = std::fs::remove_file(path);
        }
        // Case `setting_up_a_shared_profile_keeps_its_id_and_adds_nothing`.
        {
            let mut app = live_with_sample();
            let before = Vault::of(&app.state()).connections.len();
            let home = Vault::of(&app.state())
                .connections
                .iter()
                .find(|c| c.name == "Home")
                .unwrap()
                .id;
            let path = provider_file("setup", "setup-secret", "setup.example:51820");
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            let set_up = ui::host::scoped(&services, || {
                app.effect(&VpnEffect::ProvisionLocal {
                    connection: home.to_string(),
                })
            });
            assert!(set_up.is_ok(), "{set_up:?}");
            app.settle();
            let state = Vault::of(&app.state());
            assert_eq!(state.connections.len(), before, "no second shared profile");
            assert!(state.get(home).is_some(), "the original stable id remains");
            let saved = app.read_document(|doc| doc.clone().save());
            let saved = String::from_utf8_lossy(&saved);
            assert!(!saved.contains("setup-secret"));
            assert!(!saved.contains("setup.example"));
            let _ = std::fs::remove_file(path);
        }
        // Case `a_closed_picker_cancels_the_import`.
        {
            let mut app = TestApp::<VpnShell>::new().unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            let cancelled = ui::host::scoped(&services, || app.effect(&VpnEffect::ImportOpenvpn));
            assert!(
                matches!(&cancelled, Err(error) if error.to_string().contains("cancelled")),
                "{cancelled:?}"
            );
            assert!(Vault::of(&app.state()).connections.is_empty());
        }
    }
    #[test]
    fn every_scenario_stages_the_fleet_and_its_stand_in_device() {
        for scenario in VpnShell::scenarios() {
            let app = TestApp::<VpnShell>::capture(Some(scenario)).unwrap();
            let expected = if *scenario == "empty" { 0 } else { 4 };
            assert_eq!(
                Vault::of(&app.state()).connections.len(),
                expected,
                "{scenario}"
            );
        }

        // The stand-in device's tunnel states are what the list shows.
        let mut connecting = TestApp::<VpnShell>::capture(Some("connecting")).unwrap();
        let shown = connecting.harness().inspection_json().unwrap();
        assert!(shown.contains("Home, WireGuard, Connected"), "{shown}");
        assert!(shown.contains("Work, OpenVPN, Connecting"), "{shown}");

        // 'Home' is not set up on the stand-in device: it cannot connect.
        let mut unprovisioned = TestApp::<VpnShell>::capture(Some("unprovisioned")).unwrap();
        let home = Vault::of(&unprovisioned.state())
            .connections
            .iter()
            .find(|c| c.name == "Home")
            .unwrap()
            .id;
        let row = published_row(&mut unprovisioned, home);
        let connect = row
            .actions
            .iter()
            .find(|action| action.id == "connect")
            .unwrap();
        assert!(matches!(
            connect.contract.availability,
            ui::tilo_actions::Availability::Unavailable { ref code, .. } if code == "local_credentials_required"
        ));

        // Everywhere else 'Home' is up, so it can be disconnected.
        let mut detail = TestApp::<VpnShell>::capture(Some("detail")).unwrap();
        let row = published_row(&mut detail, home);
        let disconnect = row
            .actions
            .iter()
            .find(|action| action.id == "disconnect")
            .unwrap();
        assert_eq!(
            disconnect.contract.availability,
            ui::tilo_actions::Availability::Available
        );
    }
    fn framed((app, doc): (VpnApp, AutoCommit), w: f64, h: f64) -> Framed<VpnApp> {
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc));
        Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
    }
    fn target(app: &Framed<VpnApp>, id: &str) -> Rect {
        Framed::harness(app)
            .targets()
            .get(id)
            .copied()
            .unwrap_or_else(|| panic!("no widget {id}"))
    }
    fn click_at(app: &mut Framed<VpnApp>, point: Point) {
        Framed::harness_mut(app).click_at(point.x, point.y);
        Framed::harness_mut(app).relayout();
    }
    fn key(app: &mut Framed<VpnApp>, spec: &str) {
        Framed::harness_mut(app).key(spec);
        Framed::harness_mut(app).relayout();
    }
    fn demo_with_doc() -> (VpnApp, AutoCommit) {
        (sample(), AutoCommit::new())
    }
    fn id_of(app: &VpnApp, name: &str) -> ConnId {
        app.vault
            .connections
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no connection {name}"))
            .id
    }
    #[test]
    fn vpn_layout_scales_scrolls_and_keeps_controls_in_band() {
        // Wide: the list and the detail side by side, the connected tunnel
        // selected and its throughput measured.
        {
            let app = framed(demo_with_doc(), 1280.0, 800.0);
            let geometry = app.shell.geometry();
            assert!(app.shell.has_overview());
            assert!(geometry.stack.is_empty(), "nothing is pushed when wide");
            assert!(geometry.overview_column.width() > 0.0);
            assert!(geometry.content_column.x0 >= geometry.overview_column.x1);
            let home = id_of(&app, "Home");
            assert_eq!(app.vault.selected, Some(home));
            let connection = app.vault.get(home).unwrap();
            assert!(app.rates_for(connection).is_live());
            assert!(target(&app, "vpn-main-action").width() > 0.0);
        }
        // Phone: the list is the first screen; choosing a connection pushes
        // its detail, and Back returns.
        {
            let mut app = framed(demo_with_doc(), 520.0, 820.0);
            assert!(!app.shell.geometry().stack.is_empty(), "a phone pushes");
            let work = id_of(&app, "Work");
            let row = target(&app, &format!("vpn-connection-{work}"));
            click_at(&mut app, row.center());
            assert_eq!(app.vault.selected, Some(work));
            assert_eq!(app.shell.front_pane(), NavPane::Content);
            assert!(app.shell.back());
            assert_eq!(app.shell.front_pane(), NavPane::Overview);
        }
        // Nothing saved: no list, one empty state with its one action.
        {
            let mut app = VpnApp::new();
            let doc = AutoCommit::new();
            app.refresh_view();
            assert!(!app.shell.has_overview());
            let mut app = framed((app, doc), 1280.0, 800.0);
            let action = Framed::harness(&app)
                .targets()
                .into_iter()
                .find(|(name, _)| name.ends_with("EmptyState"))
                .map(|(_, rect)| rect)
                .expect("an empty state");
            assert!(action.width() > 0.0);
            app.open_add(ConnKind::WireGuard);
            app.refresh_sheet();
            assert!(app.dialog.is_some(), "the import sheet opens");
        }
    }
    #[test]
    fn vpn_connection_transitions_are_honest_and_dismissable() {
        // Case `confirm_dialog_routes_through_modaldialog`:
        // The action confirm routes through the shared `ModalDialog`: Escape
        // dismisses it, and a click on its primary button runs the confirmed
        // action — both driven through the production `handle_event`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_VPN_ALLOW_EXEC");
            let mut app = framed(demo_with_doc(), 1280.0, 800.0);
            let work = id_of(&app, "Work");
            app.select(work);
            app.request_toggle(work);
            Framed::harness_mut(&mut app).relayout();
            assert_eq!(app.modal, Modal::Confirm);
            assert!(app.dialog.as_ref().is_some_and(ModalDialog::is_open));

            key(&mut app, "Escape");
            assert_eq!(app.modal, Modal::None, "Escape closed the confirm sheet");
            assert!(app.pending.is_none(), "Escape cleared the pending action");
            assert!(app.dialog.is_none());

            app.request_toggle(work);
            Framed::harness_mut(&mut app).relayout();
            let primary = app.dialog.as_ref().unwrap().primary_rect();
            assert!(primary.width() > 0.0, "the sheet was presented");
            click_at(&mut app, primary.center());
            assert_eq!(app.modal, Modal::None, "the primary click resolved it");
            assert!(app.pending.is_none(), "confirming consumed the action");
        }
        // Case `connecting_profile_rejects_a_duplicate_transition`.
        {
            let mut app = sample();
            let id = app.vault.selected.expect("a selected profile");
            app.transitions.insert(id);

            app.request_toggle(id);

            assert_eq!(app.modal, Modal::None, "no second confirmation is opened");
            assert!(
                app.activity
                    .last()
                    .is_some_and(|line| line.contains("already changing state")),
                "the rejected request is visible in the local activity history"
            );
        }
        {
            let mut app = framed(demo_with_doc(), 1280.0, 800.0);
            app.operation_error = Some("The profile is incomplete".into());
            app.retry_import_kind = Some(ConnKind::WireGuard);
            Framed::harness_mut(&mut app).relayout();
            key(&mut app, "Escape");
            assert!(app.operation_error.is_none());
            assert!(app.retry_import_kind.is_none());
        }
        // Case `a_refused_connect_stays_disconnected`:
        // Honesty: when the command did not run, the tunnel must NOT show as
        // connected and the caller is told why.
        {
            let mut app = sample();
            let work = named(&app, "Work");
            let connection = app.vault.get(work).unwrap().clone();
            let plan = config::connect_local_plan(&connection, "uuid");
            app.transitions.insert(work);
            app.live_status.insert(work, ConnStatus::Connecting);
            let answer = app.finish_transition(work, true, &plan, config::execute(&plan), None);
            assert!(answer.is_err());
            assert!(!app.transitions.contains(&work));
            assert!(
                !app.vault.get(work).unwrap().is_connected(),
                "a refused command must never show as Connected"
            );
            assert!(app.operation_error.is_some());
        }
        // Case `readiness_not_seen_is_not_connected`.
        {
            let mut app = sample();
            let work = named(&app, "Work");
            let connection = app.vault.get(work).unwrap().clone();
            let plan = config::connect_local_plan(&connection, "uuid");
            let answer =
                app.finish_transition(work, true, &plan, Ok(String::new()), Some(Ok(false)));
            assert!(answer.is_err());
            assert_eq!(
                app.vault.get(work).unwrap().status,
                ConnStatus::Disconnected
            );
        }
    }
    #[test]
    fn add_modal_hit_tests_painted_controls() {
        for width in [1200.0, 480.0] {
            let mut app = framed(demo_with_doc(), width, 800.0);
            app.open_add(ConnKind::WireGuard);
            Framed::harness_mut(&mut app).relayout();
            let segments = target(&app, "vpn-import-kind");
            let card = app.dialog.as_ref().unwrap().dialog_rect();
            assert!(segments.width() > 0.0 && segments.x1 <= card.x1);
            click_at(
                &mut app,
                Point::new(segments.x0 + segments.width() * 0.75, segments.center().y),
            );
            assert_eq!(app.add_kind, ConnKind::OpenVpn);
            click_at(
                &mut app,
                Point::new(segments.x0 + segments.width() * 0.25, segments.center().y),
            );
            assert_eq!(app.add_kind, ConnKind::WireGuard);
            let before = app.vault.connections.len();
            let primary = app.dialog.as_ref().unwrap().primary_rect();
            let secondary = app.dialog.as_ref().unwrap().secondary_rect();
            assert!(secondary.x1 <= primary.x0);
            click_at(&mut app, primary.center());
            assert_eq!(app.vault.connections.len(), before);
            assert_eq!(app.modal, Modal::None);
            assert!(
                app.activity
                    .last()
                    .is_some_and(|line| line.contains("WireGuard provider profile"))
            );
            app.open_add(ConnKind::OpenVpn);
            Framed::harness_mut(&mut app).relayout();
            let secondary = app.dialog.as_ref().unwrap().secondary_rect();
            click_at(&mut app, secondary.center());
            assert_eq!(app.modal, Modal::None);
            assert_eq!(app.vault.connections.len(), before);
        }
    }
    #[test]
    fn the_import_sheet_imports_through_the_published_effect() {
        let mut app = TestApp::<VpnShell>::new().unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Cancelled);
        let _scope = ui::host::enter(&services);
        let harness = app.harness();
        let empty = harness
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some("Import configuration"))
            .find_map(|(_, node)| node.bounds())
            .expect("the empty state's actual import action is exposed");
        harness.click_at(
            f64::midpoint(empty.x0, empty.x1),
            f64::midpoint(empty.y0, empty.y1),
        );
        harness.relayout();
        let kinds = *harness.targets().get("vpn-import-kind").unwrap();
        harness.click_at(kinds.x0 + kinds.width() * 0.75, kinds.center().y);
        harness.relayout();
        let primary = harness
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some("Choose file"))
            .find_map(|(_, node)| node.bounds())
            .expect("the modal's primary file action is exposed");
        harness.click_at(
            f64::midpoint(primary.x0, primary.x1),
            f64::midpoint(primary.y0, primary.y1),
        );
        assert!(harness.wait_until(tilo_test_env::hang_bound(), |_| {
            !recorder.pick_requests().is_empty()
        }));
        let filters: Vec<String> = recorder.pick_requests()[0]
            .filter_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(filters, ["OpenVPN (.ovpn)", "All files"]);
        assert!(Vault::of(&app.state()).connections.is_empty());
    }
}
